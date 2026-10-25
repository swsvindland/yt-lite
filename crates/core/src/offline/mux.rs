//! Remuxes YouTube's DASH streams into one ordinary MP4.
//!
//! A download is an H.264 video stream and an AAC audio stream, each a
//! fragmented MP4: a `moov` without samples, then `moof` + `mdat` pairs.
//! [`mux`] writes them as one non-fragmented MP4 with the `moov` first, a
//! file AVPlayer, ExoPlayer and desktop players all open and seek offline.
//! Nothing is decoded: the samples are copied as they are, each fragment's
//! run (`trun`) becomes a chunk, and the runs' durations, sizes, flags and
//! composition offsets become the sample tables (`stts`, `stsz`, `stss`,
//! `ctts`, `stsc`, `stco`).

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, ensure};

/// The output's movie timescale (milliseconds, as ffmpeg writes).
const MOVIE_TIMESCALE: u32 = 1000;
/// `moov` and `moof` boxes are read into memory; real ones are kilobytes.
const MAX_HEADER_BOX: u64 = 64 << 20;

/// What [`mux`] wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Muxed {
    pub bytes: u64,
    pub duration_secs: f64,
}

/// Writes the tracks of `inputs` (fragmented MP4 files with one track each,
/// in the order given) to `output` as one MP4.
pub fn mux(inputs: &[&Path], output: &Path) -> Result<Muxed> {
    ensure!(!inputs.is_empty(), "nothing to mux");
    let mut files = Vec::new();
    let mut tracks = Vec::new();
    for (input, path) in inputs.iter().enumerate() {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let track =
            read_track(&mut file, input).with_context(|| format!("reading {}", path.display()))?;
        tracks.push(track);
        files.push(file);
    }
    let layout = interleave(&tracks);
    let payload: u64 = layout.iter().map(|&(t, c)| tracks[t].chunks[c].len).sum();
    let ftyp = ftyp(&tracks);
    let mdat_header: u64 = if payload + 8 > u64::from(u32::MAX) {
        16
    } else {
        8
    };
    // The moov's size depends only on whether its chunk offsets need 64 bits.
    let mut co64 = false;
    let mut moov_len = moov(&tracks, &layout, 0, co64).len();
    if (ftyp.len() + moov_len) as u64 + mdat_header + payload > u64::from(u32::MAX) {
        co64 = true;
        moov_len = moov(&tracks, &layout, 0, co64).len();
    }
    let base = (ftyp.len() + moov_len) as u64 + mdat_header;
    let moov = moov(&tracks, &layout, base, co64);
    debug_assert_eq!(moov.len(), moov_len);

    let out = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, out);
    w.write_all(&ftyp)?;
    w.write_all(&moov)?;
    if mdat_header == 16 {
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&(payload + 16).to_be_bytes())?;
    } else {
        w.write_all(&((payload + 8) as u32).to_be_bytes())?;
        w.write_all(b"mdat")?;
    }
    for &(t, c) in &layout {
        let chunk = &tracks[t].chunks[c];
        let file = &mut files[tracks[t].input];
        file.seek(SeekFrom::Start(chunk.offset))?;
        let copied = std::io::copy(&mut Read::by_ref(file).take(chunk.len), &mut w)?;
        ensure!(copied == chunk.len, "input ends inside a sample");
    }
    let out = w.into_inner().map_err(|e| e.into_error())?;
    out.sync_all()?;
    let duration = tracks.iter().map(Track::movie_duration).max().unwrap_or(0);
    Ok(Muxed {
        bytes: base + payload,
        duration_secs: duration as f64 / f64::from(MOVIE_TIMESCALE),
    })
}

/// One input's track: what its `moov` says, and the samples of its fragments.
struct Track {
    input: usize,
    /// In the input file; fragments refer to it.
    id: u32,
    handler: [u8; 4],
    timescale: u32,
    language: u16,
    header: TrackHeader,
    /// Where presentation starts in the media (the edit list's media time).
    media_time: i64,
    /// Seconds of nothing before it starts (empty edits).
    delay: f64,
    /// Decode time of the first fragment, which the output starts from:
    /// anything after it is a delay.
    origin: u64,
    /// Copied verbatim: `hdlr`, `vmhd`/`smhd`, `dinf`, `stsd`.
    hdlr: Vec<u8>,
    media_header: Vec<u8>,
    dinf: Vec<u8>,
    stsd: Vec<u8>,
    defaults: Defaults,
    samples: Vec<Sample>,
    /// One per run, in decode order.
    chunks: Vec<Chunk>,
    /// Decode time of the next sample, from `origin`.
    decode_end: u64,
}

#[derive(Clone, Copy, Debug)]
struct TrackHeader {
    layer: u16,
    alternate_group: u16,
    volume: u16,
    matrix: [u8; 36],
    width: u32,
    height: u32,
}

/// `trex` defaults for samples the fragments don't describe.
#[derive(Clone, Copy, Debug, Default)]
struct Defaults {
    duration: u32,
    size: u32,
    flags: u32,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    duration: u32,
    size: u32,
    /// Composition time offset.
    cto: i32,
    sync: bool,
}

#[derive(Clone, Copy, Debug)]
struct Chunk {
    /// In the input file.
    offset: u64,
    len: u64,
    samples: u32,
    decode_time: u64,
}

impl Track {
    fn media_duration(&self) -> u64 {
        self.samples.iter().map(|s| u64::from(s.duration)).sum()
    }

    /// Presented duration, in media timescale.
    fn presented(&self) -> u64 {
        self.media_duration()
            .saturating_sub(u64::try_from(self.media_time).unwrap_or(0))
    }

    /// When the first sample plays, in seconds.
    fn start(&self) -> f64 {
        self.delay + self.origin as f64 / f64::from(self.timescale)
    }

    fn delay_movie(&self) -> u64 {
        (self.start() * f64::from(MOVIE_TIMESCALE)).round() as u64
    }

    fn movie_duration(&self) -> u64 {
        self.delay_movie() + to_movie(self.presented(), self.timescale)
    }

    /// Lines the next fragment up with its `tfdt`. A gap stretches the
    /// sample before it (rather than shifting everything after); overlaps
    /// are ignored.
    fn align(&mut self, decode_time: u64) {
        if self.samples.is_empty() {
            self.origin = decode_time;
            return;
        }
        let t = decode_time.saturating_sub(self.origin);
        if t > self.decode_end
            && let Some(last) = self.samples.last_mut()
            && let Some(d) = u32::try_from(t - self.decode_end)
                .ok()
                .and_then(|gap| last.duration.checked_add(gap))
        {
            last.duration = d;
            self.decode_end = t;
        }
    }
}

fn to_movie(t: u64, timescale: u32) -> u64 {
    if timescale == 0 {
        return 0;
    }
    ((u128::from(t) * u128::from(MOVIE_TIMESCALE) + u128::from(timescale) / 2)
        / u128::from(timescale)) as u64
}

// Reading

/// A top-level box in a file.
struct Header {
    typ: [u8; 4],
    start: u64,
    header_len: u64,
    size: u64,
}

fn read_header(file: &mut File, start: u64, file_len: u64) -> Result<Header> {
    file.seek(SeekFrom::Start(start))?;
    let mut b = [0u8; 8];
    file.read_exact(&mut b)?;
    let typ = [b[4], b[5], b[6], b[7]];
    let (header_len, size) = match u32::from_be_bytes([b[0], b[1], b[2], b[3]]) {
        1 => {
            let mut large = [0u8; 8];
            file.read_exact(&mut large)?;
            (16, u64::from_be_bytes(large))
        }
        0 => (8, file_len - start),
        n => (8, u64::from(n)),
    };
    ensure!(
        size >= header_len && start + size <= file_len,
        "{} box at {start} runs past the end of the file (incomplete download?)",
        fourcc(&typ)
    );
    Ok(Header {
        typ,
        start,
        header_len,
        size,
    })
}

fn read_body(file: &mut File, h: &Header) -> Result<Vec<u8>> {
    let len = h.size - h.header_len;
    ensure!(len <= MAX_HEADER_BOX, "{} box is too large", fourcc(&h.typ));
    file.seek(SeekFrom::Start(h.start + h.header_len))?;
    let mut body = vec![0u8; len as usize];
    file.read_exact(&mut body)?;
    Ok(body)
}

fn read_track(file: &mut File, input: usize) -> Result<Track> {
    let file_len = file.metadata()?.len();
    let mut track: Option<Track> = None;
    let mut pos = 0;
    while pos + 8 <= file_len {
        let h = read_header(file, pos, file_len)?;
        match &h.typ {
            b"moov" => track = Some(parse_moov(&read_body(file, &h)?, input)?),
            b"moof" => {
                let track = track
                    .as_mut()
                    .ok_or_else(|| anyhow!("fragment before moov"))?;
                parse_moof(&read_body(file, &h)?, h.start, track)?;
            }
            _ => {}
        }
        pos = h.start + h.size;
    }
    let track = track.ok_or_else(|| anyhow!("no moov box: not an MP4 file"))?;
    ensure!(
        !track.samples.is_empty(),
        "no fragments with samples (not a DASH stream?)"
    );
    if let Some(c) = track.chunks.iter().find(|c| c.offset + c.len > file_len) {
        return Err(anyhow!(
            "samples at {}..{} are past the end of the file ({file_len} bytes)",
            c.offset,
            c.offset + c.len
        ));
    }
    Ok(track)
}

/// Reads big-endian fields from a box body.
struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or_else(|| anyhow!("box is truncated"))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(drop)
    }

    fn left(&self) -> usize {
        self.b.len() - self.p
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }

    /// A full box's version and flags.
    fn version_flags(&mut self) -> Result<(u8, u32)> {
        let vf = self.u32()?;
        Ok(((vf >> 24) as u8, vf & 0x00FF_FFFF))
    }
}

/// A box inside another box's body.
struct Child<'a> {
    typ: [u8; 4],
    /// Header included, to copy it as it is.
    whole: &'a [u8],
    body: &'a [u8],
}

fn children(body: &[u8]) -> Result<Vec<Child<'_>>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= body.len() {
        let typ = [body[p + 4], body[p + 5], body[p + 6], body[p + 7]];
        let (header_len, size) = match u32::from_be_bytes(body[p..p + 4].try_into()?) {
            1 => {
                let large = body
                    .get(p + 8..p + 16)
                    .ok_or_else(|| anyhow!("truncated {} box", fourcc(&typ)))?;
                (16, usize::try_from(u64::from_be_bytes(large.try_into()?))?)
            }
            0 => (8, body.len() - p),
            n => (8, n as usize),
        };
        ensure!(
            size >= header_len && p + size <= body.len(),
            "malformed {} box",
            fourcc(&typ)
        );
        out.push(Child {
            typ,
            whole: &body[p..p + size],
            body: &body[p + header_len..p + size],
        });
        p += size;
    }
    Ok(out)
}

fn child<'a>(body: &'a [u8], typ: &[u8; 4]) -> Result<Option<Child<'a>>> {
    Ok(children(body)?.into_iter().find(|c| &c.typ == typ))
}

fn need<'a>(body: &'a [u8], typ: &[u8; 4]) -> Result<Child<'a>> {
    child(body, typ)?.ok_or_else(|| anyhow!("no {} box", fourcc(typ)))
}

fn parse_moov(moov: &[u8], input: usize) -> Result<Track> {
    let mut movie_timescale = MOVIE_TIMESCALE;
    if let Some(mvhd) = child(moov, b"mvhd")? {
        let mut r = R::new(mvhd.body);
        let (v, _) = r.version_flags()?;
        r.skip(if v == 1 { 16 } else { 8 })?;
        movie_timescale = r.u32()?;
    }
    let traks: Vec<Child> = children(moov)?
        .into_iter()
        .filter(|c| &c.typ == b"trak")
        .collect();
    ensure!(
        traks.len() == 1,
        "expected one track, found {}",
        traks.len()
    );
    let trak = traks[0].body;

    let mut r = R::new(need(trak, b"tkhd")?.body);
    let (v, _) = r.version_flags()?;
    r.skip(if v == 1 { 16 } else { 8 })?;
    let id = r.u32()?;
    r.skip(4)?;
    r.skip(if v == 1 { 8 } else { 4 })?;
    r.skip(8)?;
    let header = TrackHeader {
        layer: r.u16()?,
        alternate_group: r.u16()?,
        volume: r.u16()?,
        matrix: {
            r.skip(2)?;
            r.take(36)?.try_into()?
        },
        width: r.u32()?,
        height: r.u32()?,
    };

    let mut media_time = None;
    let mut delay = 0.0;
    if let Some(edts) = child(trak, b"edts")?
        && let Some(elst) = child(edts.body, b"elst")?
    {
        let mut r = R::new(elst.body);
        let (v, _) = r.version_flags()?;
        for _ in 0..r.u32()? {
            let (duration, time) = if v == 1 {
                (r.u64()?, r.i64()?)
            } else {
                (u64::from(r.u32()?), i64::from(r.i32()?))
            };
            r.skip(4)?;
            if media_time.is_some() {
                break;
            }
            if time == -1 {
                delay += duration as f64 / f64::from(movie_timescale.max(1));
            } else {
                media_time = Some(time);
            }
        }
    }

    let mdia = need(trak, b"mdia")?.body;
    let mut r = R::new(need(mdia, b"mdhd")?.body);
    let (v, _) = r.version_flags()?;
    r.skip(if v == 1 { 16 } else { 8 })?;
    let timescale = r.u32()?;
    r.skip(if v == 1 { 8 } else { 4 })?;
    let language = r.u16()?;
    ensure!(timescale > 0, "track has no timescale");

    let hdlr = need(mdia, b"hdlr")?;
    let mut r = R::new(hdlr.body);
    r.skip(8)?;
    let handler: [u8; 4] = r.take(4)?.try_into()?;
    let minf = need(mdia, b"minf")?.body;
    let media_header = [b"vmhd", b"smhd", b"nmhd"]
        .into_iter()
        .find_map(|t| child(minf, t).ok().flatten())
        .ok_or_else(|| anyhow!("no media header box"))?;
    let dinf = match child(minf, b"dinf")? {
        Some(d) => d.whole.to_vec(),
        None => self_contained_dinf(),
    };
    let stsd = need(need(minf, b"stbl")?.body, b"stsd")?;

    let mut defaults = Defaults::default();
    if let Some(mvex) = child(moov, b"mvex")? {
        for trex in children(mvex.body)?.iter().filter(|c| &c.typ == b"trex") {
            let mut r = R::new(trex.body);
            r.version_flags()?;
            if r.u32()? == id {
                r.skip(4)?;
                defaults = Defaults {
                    duration: r.u32()?,
                    size: r.u32()?,
                    flags: r.u32()?,
                };
            }
        }
    }

    Ok(Track {
        input,
        id,
        handler,
        timescale,
        language,
        header,
        media_time: media_time.unwrap_or(0).max(0),
        delay,
        hdlr: hdlr.whole.to_vec(),
        media_header: media_header.whole.to_vec(),
        dinf,
        stsd: stsd.whole.to_vec(),
        defaults,
        samples: Vec::new(),
        chunks: Vec::new(),
        origin: 0,
        decode_end: 0,
    })
}

const BASE_DATA_OFFSET: u32 = 0x1;
const SAMPLE_DESCRIPTION_INDEX: u32 = 0x2;
const DEFAULT_DURATION: u32 = 0x8;
const DEFAULT_SIZE: u32 = 0x10;
const DEFAULT_FLAGS: u32 = 0x20;
const DURATION_IS_EMPTY: u32 = 0x1_0000;
const DEFAULT_BASE_IS_MOOF: u32 = 0x2_0000;

const DATA_OFFSET: u32 = 0x1;
const FIRST_SAMPLE_FLAGS: u32 = 0x4;
const SAMPLE_DURATION: u32 = 0x100;
const SAMPLE_SIZE: u32 = 0x200;
const SAMPLE_FLAGS: u32 = 0x400;
const SAMPLE_CTO: u32 = 0x800;
/// `sample_is_non_sync_sample` in sample flags.
const NON_SYNC: u32 = 0x1_0000;

/// Adds a fragment's samples to `track`. `moof_start`: the box's offset in
/// the file, which data offsets are relative to.
fn parse_moof(moof: &[u8], moof_start: u64, track: &mut Track) -> Result<()> {
    // Without an explicit base, a track fragment's data follows the previous one's.
    let mut previous_end = moof_start;
    for traf in children(moof)?.iter().filter(|c| &c.typ == b"traf") {
        let mut r = R::new(need(traf.body, b"tfhd")?.body);
        let (_, flags) = r.version_flags()?;
        let id = r.u32()?;
        let base_offset = if flags & BASE_DATA_OFFSET != 0 {
            Some(r.u64()?)
        } else {
            None
        };
        if flags & SAMPLE_DESCRIPTION_INDEX != 0 {
            r.skip(4)?;
        }
        let d = track.defaults;
        let duration = if flags & DEFAULT_DURATION != 0 {
            r.u32()?
        } else {
            d.duration
        };
        let size = if flags & DEFAULT_SIZE != 0 {
            r.u32()?
        } else {
            d.size
        };
        let sample_flags = if flags & DEFAULT_FLAGS != 0 {
            r.u32()?
        } else {
            d.flags
        };
        let base = base_offset.unwrap_or(if flags & DEFAULT_BASE_IS_MOOF != 0 {
            moof_start
        } else {
            previous_end
        });
        if id != track.id || flags & DURATION_IS_EMPTY != 0 {
            continue;
        }
        if let Some(tfdt) = child(traf.body, b"tfdt")? {
            let mut r = R::new(tfdt.body);
            let (v, _) = r.version_flags()?;
            track.align(if v == 1 {
                r.u64()?
            } else {
                u64::from(r.u32()?)
            });
        }

        let mut data = base;
        for trun in children(traf.body)?.iter().filter(|c| &c.typ == b"trun") {
            let mut r = R::new(trun.body);
            let (v, tf) = r.version_flags()?;
            let count = r.u32()?;
            if tf & DATA_OFFSET != 0 {
                data = base
                    .checked_add_signed(i64::from(r.i32()?))
                    .ok_or_else(|| anyhow!("negative data offset"))?;
            }
            let first_flags = if tf & FIRST_SAMPLE_FLAGS != 0 {
                Some(r.u32()?)
            } else {
                None
            };
            let per_sample = [SAMPLE_DURATION, SAMPLE_SIZE, SAMPLE_FLAGS, SAMPLE_CTO]
                .iter()
                .filter(|&&f| tf & f != 0)
                .count()
                * 4;
            ensure!(
                per_sample == 0 || count as usize <= r.left() / per_sample,
                "trun lists more samples than it has room for"
            );
            let first = track.samples.len();
            let chunk = Chunk {
                offset: data,
                len: 0,
                samples: count,
                decode_time: track.decode_end,
            };
            let mut len = 0u64;
            for i in 0..count {
                let duration = if tf & SAMPLE_DURATION != 0 {
                    r.u32()?
                } else {
                    duration
                };
                let size = if tf & SAMPLE_SIZE != 0 {
                    r.u32()?
                } else {
                    size
                };
                let per_sample_flags = if tf & SAMPLE_FLAGS != 0 {
                    Some(r.u32()?)
                } else {
                    None
                };
                let flags = match (i, first_flags) {
                    (0, Some(f)) => f,
                    _ => per_sample_flags.unwrap_or(sample_flags),
                };
                let cto = if tf & SAMPLE_CTO == 0 {
                    0
                } else if v == 0 {
                    i32::try_from(r.u32()?).unwrap_or(i32::MAX)
                } else {
                    r.i32()?
                };
                track.samples.push(Sample {
                    duration,
                    size,
                    cto,
                    sync: flags & NON_SYNC == 0,
                });
                len += u64::from(size);
                track.decode_end += u64::from(duration);
            }
            if track.samples.len() > first {
                track.chunks.push(Chunk { len, ..chunk });
            }
            data += len;
        }
        previous_end = data;
    }
    Ok(())
}

/// A `dinf` saying the media is in this file.
fn self_contained_dinf() -> Vec<u8> {
    let mut w = W::default();
    let dinf = w.begin(b"dinf");
    let dref = w.full(b"dref", 0, 0);
    w.u32(1);
    let url = w.full(b"url ", 0, 1);
    w.end(url);
    w.end(dref);
    w.end(dinf);
    w.0
}

fn fourcc(t: &[u8; 4]) -> String {
    String::from_utf8_lossy(t).into_owned()
}

// Writing

/// The mdat order: every track's chunks by decode time, so players read
/// the file front to back.
fn interleave(tracks: &[Track]) -> Vec<(usize, usize)> {
    let mut order: Vec<(f64, usize, usize)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(t, track)| {
            track.chunks.iter().enumerate().map(move |(c, chunk)| {
                let at = track.start() + chunk.decode_time as f64 / f64::from(track.timescale);
                (at, t, c)
            })
        })
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    order.into_iter().map(|(_, t, c)| (t, c)).collect()
}

fn ftyp(tracks: &[Track]) -> Vec<u8> {
    let mut w = W::default();
    let at = w.begin(b"ftyp");
    w.bytes(b"isom");
    w.u32(0x200);
    w.bytes(b"isomiso2");
    if tracks.iter().any(|t| &t.handler == b"vide") {
        w.bytes(b"avc1");
    }
    w.bytes(b"mp41");
    w.end(at);
    w.0
}

/// `base`: where the first chunk's data starts in the output.
fn moov(tracks: &[Track], layout: &[(usize, usize)], base: u64, co64: bool) -> Vec<u8> {
    let mut offsets: Vec<Vec<u64>> = tracks.iter().map(|t| vec![0; t.chunks.len()]).collect();
    let mut pos = base;
    for &(t, c) in layout {
        offsets[t][c] = pos;
        pos += tracks[t].chunks[c].len;
    }
    let mut w = W::default();
    let moov = w.begin(b"moov");
    let duration = tracks.iter().map(Track::movie_duration).max().unwrap_or(0);
    mvhd(&mut w, duration, tracks.len() as u32 + 1);
    for (i, track) in tracks.iter().enumerate() {
        trak(&mut w, track, i as u32 + 1, &offsets[i], co64);
    }
    w.end(moov);
    w.0
}

const IDENTITY: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn mvhd(w: &mut W, duration: u64, next_track_id: u32) {
    let v1 = duration > u64::from(u32::MAX);
    let at = w.full(b"mvhd", v1.into(), 0);
    w.times(v1);
    w.u32(MOVIE_TIMESCALE);
    w.duration(v1, duration);
    w.u32(0x0001_0000); // rate 1.0
    w.u16(0x0100); // volume 1.0
    w.zeros(10);
    for m in IDENTITY {
        w.u32(m);
    }
    w.zeros(24);
    w.u32(next_track_id);
    w.end(at);
}

fn trak(w: &mut W, t: &Track, id: u32, offsets: &[u64], co64: bool) {
    let trak = w.begin(b"trak");

    let duration = t.movie_duration();
    let v1 = duration > u64::from(u32::MAX);
    let tkhd = w.full(b"tkhd", v1.into(), 0x3); // enabled, in movie
    w.times(v1);
    w.u32(id);
    w.u32(0);
    w.duration(v1, duration);
    w.zeros(8);
    w.u16(t.header.layer);
    w.u16(t.header.alternate_group);
    w.u16(t.header.volume);
    w.u16(0);
    w.bytes(&t.header.matrix);
    w.u32(t.header.width);
    w.u32(t.header.height);
    w.end(tkhd);

    let delay = t.delay_movie();
    if delay > 0 || t.media_time > 0 {
        let presented = to_movie(t.presented(), t.timescale);
        let mut edits = Vec::new();
        if delay > 0 {
            edits.push((delay, -1i64));
        }
        edits.push((presented, t.media_time));
        let v1 = edits
            .iter()
            .any(|&(d, m)| d > u64::from(u32::MAX) || i32::try_from(m).is_err());
        let edts = w.begin(b"edts");
        let elst = w.full(b"elst", v1.into(), 0);
        w.u32(edits.len() as u32);
        for (d, m) in edits {
            if v1 {
                w.u64(d);
                w.u64(m as u64);
            } else {
                w.u32(d as u32);
                w.u32(m as i32 as u32);
            }
            w.u32(0x0001_0000); // rate 1.0
        }
        w.end(elst);
        w.end(edts);
    }

    let mdia = w.begin(b"mdia");
    let media_duration = t.media_duration();
    let v1 = media_duration > u64::from(u32::MAX);
    let mdhd = w.full(b"mdhd", v1.into(), 0);
    w.times(v1);
    w.u32(t.timescale);
    w.duration(v1, media_duration);
    w.u16(t.language);
    w.u16(0);
    w.end(mdhd);
    w.bytes(&t.hdlr);
    let minf = w.begin(b"minf");
    w.bytes(&t.media_header);
    w.bytes(&t.dinf);
    stbl(w, t, offsets, co64);
    w.end(minf);
    w.end(mdia);

    w.end(trak);
}

fn stbl(w: &mut W, t: &Track, offsets: &[u64], co64: bool) {
    let stbl = w.begin(b"stbl");
    w.bytes(&t.stsd);

    let stts = runs(t.samples.iter().map(|s| s.duration));
    let at = w.full(b"stts", 0, 0);
    w.u32(stts.len() as u32);
    for (count, delta) in stts {
        w.u32(count);
        w.u32(delta);
    }
    w.end(at);

    if t.samples.iter().any(|s| s.cto != 0) {
        let signed = t.samples.iter().any(|s| s.cto < 0);
        let ctts = runs(t.samples.iter().map(|s| s.cto));
        let at = w.full(b"ctts", signed.into(), 0);
        w.u32(ctts.len() as u32);
        for (count, offset) in ctts {
            w.u32(count);
            w.u32(offset as u32);
        }
        w.end(at);
    }

    if t.samples.iter().any(|s| !s.sync) {
        let at = w.full(b"stss", 0, 0);
        w.u32(t.samples.iter().filter(|s| s.sync).count() as u32);
        for (i, _) in t.samples.iter().enumerate().filter(|(_, s)| s.sync) {
            w.u32(i as u32 + 1);
        }
        w.end(at);
    }

    let at = w.full(b"stsc", 0, 0);
    let mut entries = Vec::new();
    for (i, c) in t.chunks.iter().enumerate() {
        if entries.last().is_none_or(|&(_, n)| n != c.samples) {
            entries.push((i as u32 + 1, c.samples));
        }
    }
    w.u32(entries.len() as u32);
    for (first_chunk, samples) in entries {
        w.u32(first_chunk);
        w.u32(samples);
        w.u32(1); // sample description index
    }
    w.end(at);

    let at = w.full(b"stsz", 0, 0);
    let first = t.samples[0].size;
    if t.samples.iter().all(|s| s.size == first) {
        w.u32(first);
        w.u32(t.samples.len() as u32);
    } else {
        w.u32(0);
        w.u32(t.samples.len() as u32);
        for s in &t.samples {
            w.u32(s.size);
        }
    }
    w.end(at);

    let at = w.full(if co64 { b"co64" } else { b"stco" }, 0, 0);
    w.u32(offsets.len() as u32);
    for &o in offsets {
        if co64 {
            w.u64(o);
        } else {
            w.u32(o as u32);
        }
    }
    w.end(at);

    w.end(stbl);
}

/// Run-length encoding: (count, value).
fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// Builds boxes; sizes are patched in by [`W::end`].
#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn begin(&mut self, typ: &[u8; 4]) -> usize {
        let at = self.0.len();
        self.u32(0);
        self.bytes(typ);
        at
    }

    fn full(&mut self, typ: &[u8; 4], version: u8, flags: u32) -> usize {
        let at = self.begin(typ);
        self.u32(u32::from(version) << 24 | flags);
        at
    }

    fn end(&mut self, at: usize) {
        let size = (self.0.len() - at) as u32;
        self.0[at..at + 4].copy_from_slice(&size.to_be_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }

    fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0);
    }

    fn u16(&mut self, v: u16) {
        self.bytes(&v.to_be_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_be_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_be_bytes());
    }

    /// Creation and modification times (zero: unknown).
    fn times(&mut self, v1: bool) {
        self.zeros(if v1 { 16 } else { 8 });
    }

    fn duration(&mut self, v1: bool, d: u64) {
        if v1 {
            self.u64(d);
        } else {
            self.u32(d as u32);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A sample of a test input: duration, size, sync, composition offset.
    pub(crate) type S = (u32, u32, bool, i32);

    /// Byte `i` of sample `n` in input `tag`, so copies can be checked.
    fn sample_bytes(tag: u8, n: usize, size: u32) -> Vec<u8> {
        (0..size)
            .map(|i| {
                tag.wrapping_mul(31)
                    .wrapping_add(n as u8)
                    .wrapping_add(i as u8)
            })
            .collect()
    }

    /// A DASH-style fragmented MP4 like YouTube's: tfhd with
    /// default-base-is-moof, trun with a data offset and per-sample
    /// size/flags/composition offsets, `per_fragment` samples per fragment.
    pub(crate) fn fragmented(
        tag: u8,
        handler: &[u8; 4],
        timescale: u32,
        samples: &[S],
        per_fragment: usize,
        edit_media_time: Option<u32>,
    ) -> Vec<u8> {
        let mut w = W::default();
        let ftyp = w.begin(b"ftyp");
        w.bytes(b"dash\0\0\0\0iso6mp41");
        w.end(ftyp);

        let moov = w.begin(b"moov");
        mvhd(&mut w, 0, 2);
        let mvex = w.begin(b"mvex");
        let trex = w.full(b"trex", 0, 0);
        w.u32(1);
        w.u32(1);
        w.u32(0);
        w.u32(0);
        w.u32(0);
        w.end(trex);
        w.end(mvex);
        let trak = w.begin(b"trak");
        let tkhd = w.full(b"tkhd", 0, 3);
        w.times(false);
        w.u32(1);
        w.u32(0);
        w.u32(0);
        w.zeros(8);
        w.u16(0);
        w.u16(0);
        w.u16(if handler == b"soun" { 0x100 } else { 0 });
        w.u16(0);
        for m in IDENTITY {
            w.u32(m);
        }
        w.u32(1920 << 16);
        w.u32(1080 << 16);
        w.end(tkhd);
        if let Some(mt) = edit_media_time {
            let edts = w.begin(b"edts");
            let elst = w.full(b"elst", 0, 0);
            w.u32(1);
            w.u32(0);
            w.u32(mt);
            w.u32(0x0001_0000);
            w.end(elst);
            w.end(edts);
        }
        let mdia = w.begin(b"mdia");
        let mdhd = w.full(b"mdhd", 0, 0);
        w.times(false);
        w.u32(timescale);
        w.u32(0);
        w.u16(0x55c4); // "und"
        w.u16(0);
        w.end(mdhd);
        let hdlr = w.full(b"hdlr", 0, 0);
        w.u32(0);
        w.bytes(handler);
        w.zeros(12);
        w.bytes(b"Test\0");
        w.end(hdlr);
        let minf = w.begin(b"minf");
        let mh = w.full(if handler == b"soun" { b"smhd" } else { b"vmhd" }, 0, 1);
        w.zeros(if handler == b"soun" { 4 } else { 8 });
        w.end(mh);
        w.bytes(&self_contained_dinf());
        let stbl = w.begin(b"stbl");
        let stsd = w.full(b"stsd", 0, 0);
        w.u32(1);
        let entry = w.begin(if handler == b"soun" { b"mp4a" } else { b"avc1" });
        w.bytes(&[tag; 12]);
        w.end(entry);
        w.end(stsd);
        for empty in [b"stts", b"stsc", b"stco"] {
            let at = w.full(empty, 0, 0);
            w.u32(0);
            w.end(at);
        }
        let stsz = w.full(b"stsz", 0, 0);
        w.u32(0);
        w.u32(0);
        w.end(stsz);
        w.end(stbl);
        w.end(minf);
        w.end(mdia);
        w.end(trak);
        w.end(moov);

        let mut decode = 0u64;
        for (f, frag) in samples.chunks(per_fragment).enumerate() {
            let moof_start = w.0.len();
            let moof = w.begin(b"moof");
            let mfhd = w.full(b"mfhd", 0, 0);
            w.u32(f as u32 + 1);
            w.end(mfhd);
            let traf = w.begin(b"traf");
            let tfhd = w.full(b"tfhd", 0, DEFAULT_BASE_IS_MOOF | DEFAULT_DURATION);
            w.u32(1);
            w.u32(frag[0].0);
            w.end(tfhd);
            let tfdt = w.full(b"tfdt", 1, 0);
            w.u64(decode);
            w.end(tfdt);
            let trun = w.full(
                b"trun",
                1,
                DATA_OFFSET | SAMPLE_DURATION | SAMPLE_SIZE | SAMPLE_FLAGS | SAMPLE_CTO,
            );
            w.u32(frag.len() as u32);
            let data_offset_at = w.0.len();
            w.u32(0);
            for &(d, s, sync, cto) in frag {
                w.u32(d);
                w.u32(s);
                w.u32(if sync { 0x0200_0000 } else { 0x0101_0000 });
                w.u32(cto as u32);
                decode += u64::from(d);
            }
            w.end(trun);
            w.end(traf);
            w.end(moof);
            let data_offset = (w.0.len() + 8 - moof_start) as u32;
            w.0[data_offset_at..data_offset_at + 4].copy_from_slice(&data_offset.to_be_bytes());
            let mdat = w.begin(b"mdat");
            let before = f * per_fragment;
            for (i, &(_, s, _, _)) in frag.iter().enumerate() {
                w.bytes(&sample_bytes(tag, before + i, s));
            }
            w.end(mdat);
        }
        w.0
    }

    /// The sample tables of one output track.
    struct Out {
        handler: [u8; 4],
        timescale: u32,
        durations: Vec<u32>,
        ctos: Vec<i32>,
        sync: Option<Vec<u32>>,
        sizes: Vec<u32>,
        chunk_offsets: Vec<u64>,
        chunk_samples: Vec<u32>,
        elst: Option<(u64, i64)>,
        stsd: Vec<u8>,
    }

    fn read_output(file: &[u8]) -> (Vec<[u8; 4]>, u64, Vec<Out>) {
        let top = children(file).unwrap();
        let types = top.iter().map(|c| c.typ).collect();
        let moov = top.iter().find(|c| &c.typ == b"moov").unwrap().body;
        let mut r = R::new(need(moov, b"mvhd").unwrap().body);
        r.version_flags().unwrap();
        r.skip(8).unwrap();
        assert_eq!(r.u32().unwrap(), MOVIE_TIMESCALE);
        let movie_duration = u64::from(r.u32().unwrap());
        let mut out = Vec::new();
        for trak in children(moov).unwrap().iter().filter(|c| &c.typ == b"trak") {
            let mdia = need(trak.body, b"mdia").unwrap().body;
            let mut r = R::new(need(mdia, b"mdhd").unwrap().body);
            r.version_flags().unwrap();
            r.skip(8).unwrap();
            let timescale = r.u32().unwrap();
            let handler: [u8; 4] = need(mdia, b"hdlr").unwrap().body[8..12].try_into().unwrap();
            let stbl = need(need(mdia, b"minf").unwrap().body, b"stbl")
                .unwrap()
                .body;
            let table = |typ: &[u8; 4]| child(stbl, typ).unwrap().map(|c| c.body);

            let mut durations = Vec::new();
            let mut r = R::new(table(b"stts").unwrap());
            r.version_flags().unwrap();
            for _ in 0..r.u32().unwrap() {
                let (n, d) = (r.u32().unwrap(), r.u32().unwrap());
                durations.extend(std::iter::repeat_n(d, n as usize));
            }
            let mut ctos = vec![0; durations.len()];
            if let Some(ctts) = table(b"ctts") {
                ctos.clear();
                let mut r = R::new(ctts);
                r.version_flags().unwrap();
                for _ in 0..r.u32().unwrap() {
                    let (n, o) = (r.u32().unwrap(), r.i32().unwrap());
                    ctos.extend(std::iter::repeat_n(o, n as usize));
                }
            }
            let sync = table(b"stss").map(|b| {
                let mut r = R::new(b);
                r.version_flags().unwrap();
                (0..r.u32().unwrap()).map(|_| r.u32().unwrap()).collect()
            });
            let mut r = R::new(table(b"stsz").unwrap());
            r.version_flags().unwrap();
            let (uniform, count) = (r.u32().unwrap(), r.u32().unwrap());
            let sizes = (0..count)
                .map(|_| {
                    if uniform != 0 {
                        uniform
                    } else {
                        r.u32().unwrap()
                    }
                })
                .collect();
            let (chunk_offsets, wide) = match table(b"stco") {
                Some(b) => (b, false),
                None => (table(b"co64").unwrap(), true),
            };
            let mut r = R::new(chunk_offsets);
            r.version_flags().unwrap();
            let chunk_offsets: Vec<u64> = (0..r.u32().unwrap())
                .map(|_| {
                    if wide {
                        r.u64().unwrap()
                    } else {
                        u64::from(r.u32().unwrap())
                    }
                })
                .collect();
            let mut r = R::new(table(b"stsc").unwrap());
            r.version_flags().unwrap();
            let entries: Vec<(u32, u32)> = (0..r.u32().unwrap())
                .map(|_| {
                    let e = (r.u32().unwrap(), r.u32().unwrap());
                    r.skip(4).unwrap();
                    e
                })
                .collect();
            let chunk_samples = (1..=chunk_offsets.len() as u32)
                .map(|c| entries.iter().rev().find(|e| e.0 <= c).unwrap().1)
                .collect();
            let elst = child(trak.body, b"edts").unwrap().map(|edts| {
                let mut r = R::new(need(edts.body, b"elst").unwrap().body);
                r.version_flags().unwrap();
                assert_eq!(r.u32().unwrap(), 1);
                (u64::from(r.u32().unwrap()), i64::from(r.i32().unwrap()))
            });
            let stsd = child(stbl, b"stsd").unwrap().unwrap().whole.to_vec();
            out.push(Out {
                handler,
                timescale,
                durations,
                ctos,
                sync,
                sizes,
                chunk_offsets,
                chunk_samples,
                elst,
                stsd,
            });
        }
        (types, movie_duration, out)
    }

    /// Checks every sample of `track` holds input `tag`'s bytes.
    fn check_samples(file: &[u8], track: &Out, tag: u8) {
        let mut n = 0;
        for (&offset, &count) in track.chunk_offsets.iter().zip(&track.chunk_samples) {
            let mut at = offset as usize;
            for _ in 0..count {
                let size = track.sizes[n];
                assert_eq!(
                    &file[at..at + size as usize],
                    &sample_bytes(tag, n, size)[..],
                    "sample {n} of input {tag}"
                );
                at += size as usize;
                n += 1;
            }
        }
        assert_eq!(n, track.sizes.len());
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("yt-lite-mux-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn muxes_video_and_audio_into_one_progressive_mp4() {
        // 30 fps H.264-like video: a keyframe every 10 frames, B-frame
        // offsets, 1001/30000 timing, an edit list skipping one frame.
        let video: Vec<S> = (0..45)
            .map(|i| {
                (
                    1001,
                    300 + i * 7,
                    i % 10 == 0,
                    if i % 2 == 0 { 2002 } else { 0 },
                )
            })
            .collect();
        // 44.1 kHz AAC-like audio: 1024-sample frames, all sync.
        let audio: Vec<S> = (0..70).map(|i| (1024, 200 + (i % 5), true, 0)).collect();
        let dir = temp_dir("av");
        let (v, a, out) = (dir.join("v.mp4"), dir.join("a.m4a"), dir.join("out.mp4"));
        std::fs::write(&v, fragmented(1, b"vide", 30000, &video, 15, Some(1001))).unwrap();
        std::fs::write(&a, fragmented(2, b"soun", 44100, &audio, 20, None)).unwrap();

        let muxed = mux(&[&v, &a], &out).unwrap();
        let file = std::fs::read(&out).unwrap();
        assert_eq!(muxed.bytes, file.len() as u64);

        let (types, movie_duration, tracks) = read_output(&file);
        assert_eq!(
            types,
            [*b"ftyp", *b"moov", *b"mdat"],
            "moov before the data"
        );
        let [vt, at] = &tracks[..] else {
            panic!("two tracks")
        };
        assert_eq!((&vt.handler, vt.timescale), (b"vide", 30000));
        assert_eq!((&at.handler, at.timescale), (b"soun", 44100));
        assert!(
            vt.stsd.windows(4).any(|w| w == b"avc1"),
            "sample entry copied"
        );

        assert_eq!(vt.durations, video.iter().map(|s| s.0).collect::<Vec<_>>());
        assert_eq!(vt.sizes, video.iter().map(|s| s.1).collect::<Vec<_>>());
        assert_eq!(vt.ctos, video.iter().map(|s| s.3).collect::<Vec<_>>());
        assert_eq!(vt.sync.as_deref(), Some(&[1, 11, 21, 31, 41][..]));
        assert_eq!(vt.chunk_samples, [15, 15, 15]);
        // 45 frames, one of them skipped by the edit: 44 * 1001 / 30 ms.
        assert_eq!(vt.elst, Some((1468, 1001)));

        assert_eq!(at.durations, vec![1024; 70]);
        assert!(at.ctos.iter().all(|&c| c == 0));
        assert_eq!(at.sync, None, "all sync: no stss");
        assert_eq!(at.chunk_samples, [20, 20, 20, 10]);
        assert_eq!(at.elst, None);
        // 70 * 1024 / 44100 s.
        assert_eq!(movie_duration, 1625);

        check_samples(&file, vt, 1);
        check_samples(&file, at, 2);
        // Interleaved by time: audio's second chunk (0.46 s) comes before
        // video's second (0.5 s), and the data is laid out in that order.
        assert!(at.chunk_offsets[1] < vt.chunk_offsets[1]);
        let mut all: Vec<u64> = vt
            .chunk_offsets
            .iter()
            .chain(&at.chunk_offsets)
            .copied()
            .collect();
        all.sort();
        assert_eq!(all[0], vt.chunk_offsets[0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn audio_only() {
        let audio: Vec<S> = (0..10).map(|_| (1024, 64, true, 0)).collect();
        let dir = temp_dir("a");
        let (a, out) = (dir.join("a.m4a"), dir.join("out.m4a"));
        std::fs::write(&a, fragmented(3, b"soun", 48000, &audio, 4, None)).unwrap();
        mux(&[&a], &out).unwrap();
        let file = std::fs::read(&out).unwrap();
        let (_, _, tracks) = read_output(&file);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].sizes, vec![64; 10], "uniform sizes");
        check_samples(&file, &tracks[0], 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn incomplete_input_is_an_error() {
        let audio: Vec<S> = (0..10).map(|_| (1024, 64, true, 0)).collect();
        let dir = temp_dir("cut");
        let (a, out) = (dir.join("a.m4a"), dir.join("out.m4a"));
        let mut bytes = fragmented(4, b"soun", 48000, &audio, 4, None);
        bytes.truncate(bytes.len() - 10);
        std::fs::write(&a, bytes).unwrap();
        let err = mux(&[&a], &out).unwrap_err();
        assert!(format!("{err:#}").contains("past the end"), "{err:#}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gaps_stretch_the_previous_sample() {
        let mut t = Track {
            input: 0,
            id: 1,
            handler: *b"soun",
            timescale: 1000,
            language: 0,
            header: TrackHeader {
                layer: 0,
                alternate_group: 0,
                volume: 0,
                matrix: [0; 36],
                width: 0,
                height: 0,
            },
            media_time: 0,
            delay: 0.0,
            hdlr: vec![],
            media_header: vec![],
            dinf: vec![],
            stsd: vec![],
            defaults: Defaults::default(),
            samples: vec![],
            chunks: vec![],
            origin: 0,
            decode_end: 0,
        };
        t.align(500);
        assert_eq!(t.start(), 0.5, "a late first fragment is a delay");
        t.samples.push(Sample {
            duration: 20,
            size: 1,
            cto: 0,
            sync: true,
        });
        t.decode_end += 20;
        t.align(530);
        assert_eq!(t.samples[0].duration, 30);
        t.align(510);
        assert_eq!(t.decode_end, 30, "overlap ignored");
        assert_eq!(t.movie_duration(), 530);
    }
}
