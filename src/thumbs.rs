//! Thumbnails: download once to a disk cache, decode at display size into
//! BGRA, and keep decoded images in a byte-capped LRU. Evicted images must also
//! be dropped from GPUI's sprite atlas (see [`Thumbnails::finish`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use gpui_kit::{RenderImage, SharedString};
use image::{Frame, ImageFormat, imageops::FilterType};

use crate::lru::WeightedLru;
use crate::net::Http;
use crate::youtube::thumbnail_url;

/// A cap smaller than one screen of cards would evict visible thumbnails and
/// reload them forever; 16 MB holds ~70 cards at 320x180.
pub const MIN_MEMORY_CAP: usize = 16 * 1024 * 1024;

pub struct Thumbnails {
    lru: WeightedLru<SharedString, Arc<RenderImage>>,
    pending: HashSet<SharedString>,
    failed: HashSet<SharedString>,
    dir: PathBuf,
}

impl Thumbnails {
    pub fn new(dir: PathBuf, memory_cap_bytes: usize) -> Self {
        Self {
            lru: WeightedLru::new(memory_cap_bytes.max(MIN_MEMORY_CAP)),
            pending: HashSet::new(),
            failed: HashSet::new(),
            dir,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the decoded image and marks it recently used.
    pub fn get(&mut self, id: &SharedString) -> Option<Arc<RenderImage>> {
        self.lru.get(id).cloned()
    }

    /// True if a load should be started for `id`; marks it pending.
    pub fn begin_load(&mut self, id: &SharedString) -> bool {
        if self.lru.contains(id) || self.failed.contains(id) {
            return false;
        }
        self.pending.insert(id.clone())
    }

    /// Records a finished load. Returns images evicted to stay under the cap;
    /// the caller must release them from the GPU atlas (`cx.drop_image`).
    pub fn finish(&mut self, id: SharedString, result: Result<RenderImage>) -> Vec<Arc<RenderImage>> {
        self.pending.remove(&id);
        match result {
            Ok(img) => {
                let weight = image_bytes(&img);
                self.lru.insert(id, Arc::new(img), weight)
            }
            Err(e) => {
                log::debug!("thumbnail {id}: {e:#}");
                self.failed.insert(id);
                Vec::new()
            }
        }
    }

    /// Drops every decoded image (e.g. when idle). The caller must release
    /// them from the GPU atlas. In-flight loads still land afterwards.
    pub fn clear(&mut self) -> Vec<Arc<RenderImage>> {
        self.lru.drain()
    }

    /// Lets failed thumbnails retry (called on refresh).
    pub fn clear_failures(&mut self) {
        self.failed.clear();
    }

    pub fn memory_bytes(&self) -> usize {
        self.lru.weight()
    }

    pub fn len(&self) -> usize {
        self.lru.len()
    }
}

fn image_bytes(img: &RenderImage) -> usize {
    (0..img.frame_count())
        .filter_map(|i| img.as_bytes(i))
        .map(<[u8]>::len)
        .sum()
}

/// Blocking: disk cache or network, then decode to at most `max_w`x`max_h`.
pub fn load(http: &Http, dir: &Path, video_id: &str, max_w: u32, max_h: u32) -> Result<RenderImage> {
    let path = dir.join(format!("{video_id}.jpg"));
    let bytes = match std::fs::read(&path) {
        Ok(b) => {
            // Bump mtime so disk pruning is least-recently-used.
            if let Ok(f) = std::fs::File::options().append(true).open(&path) {
                let _ = f.set_modified(SystemTime::now());
            }
            b
        }
        Err(_) => {
            let b = http.get_bytes(&thumbnail_url(video_id))?;
            std::fs::create_dir_all(dir)?;
            // Write via temp file so a crash can't leave a truncated JPEG.
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &b)?;
            std::fs::rename(&tmp, &path)?;
            b
        }
    };
    decode(&bytes, max_w, max_h)
}

pub fn decode(bytes: &[u8], max_w: u32, max_h: u32) -> Result<RenderImage> {
    let img = image::load_from_memory_with_format(bytes, ImageFormat::Jpeg)
        .context("decoding thumbnail")?;
    let img = if img.width() > max_w || img.height() > max_h {
        img.resize(max_w, max_h, FilterType::Triangle)
    } else {
        img
    };
    let mut rgba = img.into_rgba8();
    // GPUI expects BGRA.
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Ok(RenderImage::new(vec![Frame::new(rgba)]))
}

/// Deletes least-recently-used files until the directory is under `cap_bytes`.
pub fn prune_disk(dir: &Path, cap_bytes: u64) -> Result<usize> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Ok(0);
    };
    let files: Vec<(PathBuf, u64, SystemTime)> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file()
                .then(|| (e.path(), meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect();
    let victims = select_for_deletion(files, cap_bytes);
    for p in &victims {
        let _ = std::fs::remove_file(p);
    }
    Ok(victims.len())
}

fn select_for_deletion(mut files: Vec<(PathBuf, u64, SystemTime)>, cap_bytes: u64) -> Vec<PathBuf> {
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.2);
    let mut out = Vec::new();
    for (path, size, _) in files {
        if total <= cap_bytes {
            break;
        }
        total -= size;
        out.push(path);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([x as u8, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn decodes_and_downscales_to_display_size() {
        let img = decode(&jpeg(640, 360), 320, 180).unwrap();
        let size = img.size(0);
        assert_eq!((size.width.0, size.height.0), (320, 180));
        assert_eq!(image_bytes(&img), 320 * 180 * 4);
        // Red/blue swapped into BGRA: source pixel is blue-ish (b=255).
        let px = &img.as_bytes(0).unwrap()[..4];
        assert!(px[0] > 200, "B channel first: {px:?}");
    }

    #[test]
    fn does_not_upscale() {
        let img = decode(&jpeg(160, 90), 320, 180).unwrap();
        assert_eq!(img.size(0).width.0, 160);
    }

    #[test]
    fn memory_cache_respects_cap_and_reports_evictions() {
        let one = MIN_MEMORY_CAP / 2;
        let mut t = Thumbnails::new(PathBuf::new(), one * 2);
        let decode = |_: &[u8], _, _| -> Result<RenderImage> {
            // A synthetic image weighing exactly `one` bytes.
            let side = ((one / 4) as f64).sqrt() as u32;
            Ok(RenderImage::new(vec![Frame::new(image::RgbaImage::new(side, side))]))
        };
        let one = {
            let img = decode(&[], 0, 0).unwrap();
            image_bytes(&img)
        };
        for id in ["a", "b", "c"] {
            let id = SharedString::from(id);
            assert!(t.begin_load(&id));
            assert!(!t.begin_load(&id), "already pending");
            let evicted = t.finish(id, decode(&[], 0, 0));
            assert!(t.memory_bytes() <= one * 2);
            if t.len() == 2 && evicted.len() == 1 {
                // third insert evicted "a"
                assert!(t.get(&"a".into()).is_none());
            }
        }
        assert_eq!(t.len(), 2);
        assert!(t.get(&"c".into()).is_some());
        assert_eq!(t.clear().len(), 2);
        assert_eq!(t.memory_bytes(), 0);
    }

    #[test]
    fn cap_has_floor() {
        let t = Thumbnails::new(PathBuf::new(), 1);
        assert_eq!(t.lru.cap(), MIN_MEMORY_CAP);
    }

    #[test]
    fn failures_are_not_retried_until_cleared() {
        let mut t = Thumbnails::new(PathBuf::new(), 1 << 20);
        let id = SharedString::from("x");
        assert!(t.begin_load(&id));
        t.finish(id.clone(), Err(anyhow::anyhow!("404")));
        assert!(!t.begin_load(&id));
        t.clear_failures();
        assert!(t.begin_load(&id));
    }

    #[test]
    fn disk_prune_removes_oldest_first() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let files = vec![
            (PathBuf::from("new"), 40, t0 + Duration::from_secs(30)),
            (PathBuf::from("old"), 40, t0),
            (PathBuf::from("mid"), 40, t0 + Duration::from_secs(10)),
        ];
        assert_eq!(select_for_deletion(files.clone(), 120), Vec::<PathBuf>::new());
        assert_eq!(select_for_deletion(files.clone(), 80), vec![PathBuf::from("old")]);
        assert_eq!(
            select_for_deletion(files, 10),
            vec![PathBuf::from("old"), PathBuf::from("mid"), PathBuf::from("new")]
        );
    }
}
