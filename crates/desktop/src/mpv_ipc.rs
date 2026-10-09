//! Follows a running mpv over its JSON IPC (a Unix socket, or a named pipe on
//! Windows) to learn how far the video got: reported every few seconds while
//! it plays, and once more when mpv exits.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::player::{Position, PositionQueue};

/// How often a playing video's position is reported.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// How long mpv gets to create its IPC endpoint.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A new IPC endpoint for one mpv process (`--input-ipc-server`).
pub fn endpoint() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "yt-lite-mpv-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    if cfg!(windows) {
        format!(r"\\.\pipe\{name}")
    } else {
        std::env::temp_dir()
            .join(format!("{name}.sock"))
            .display()
            .to_string()
    }
}

/// Starts a thread that reports `video_id`'s playback position to `queue`.
pub fn watch(endpoint: String, video_id: String, queue: PositionQueue) {
    std::thread::spawn(move || {
        let Some((reader, mut writer)) = connect(&endpoint) else {
            log::warn!("mpv IPC: no connection at {endpoint}; playback position won't be saved");
            return;
        };
        let observe = concat!(
            r#"{"command":["observe_property",1,"time-pos"]}"#,
            "\n",
            r#"{"command":["observe_property",2,"duration"]}"#,
            "\n"
        );
        if let Err(e) = writer.write_all(observe.as_bytes()) {
            log::warn!("mpv IPC: {e}");
            return;
        }
        let mut tracker = Tracker::default();
        let mut reported = Instant::now();
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            tracker.handle(&line);
            if reported.elapsed() >= REPORT_EVERY {
                reported = Instant::now();
                queue.extend(tracker.position(&video_id, false));
            }
        }
        // The connection closes when mpv exits.
        queue.extend(tracker.position(&video_id, true));
        #[cfg(unix)]
        let _ = std::fs::remove_file(&endpoint);
    });
}

type Pipe = (Box<dyn Read + Send>, Box<dyn Write + Send>);

/// mpv creates the endpoint shortly after it starts.
fn connect(endpoint: &str) -> Option<Pipe> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match open(endpoint) {
            Ok(pipe) => return Some(pipe),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                log::debug!("mpv IPC {endpoint}: {e}");
                return None;
            }
        }
    }
}

#[cfg(unix)]
fn open(endpoint: &str) -> std::io::Result<Pipe> {
    let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
    Ok((Box::new(stream.try_clone()?), Box::new(stream)))
}

#[cfg(windows)]
fn open(endpoint: &str) -> std::io::Result<Pipe> {
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(endpoint)?;
    Ok((Box::new(pipe.try_clone()?), Box::new(pipe)))
}

/// What mpv has said about playback so far.
#[derive(Default)]
struct Tracker {
    time_pos: Option<f64>,
    duration: Option<f64>,
    eof: bool,
}

impl Tracker {
    fn handle(&mut self, line: &str) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match msg["event"].as_str() {
            // `data` turns null when the file unloads; keep the last value.
            Some("property-change") => match (msg["name"].as_str(), msg["data"].as_f64()) {
                (Some("time-pos"), Some(t)) => self.time_pos = Some(t),
                (Some("duration"), Some(d)) => self.duration = Some(d),
                _ => {}
            },
            Some("end-file") => self.eof = msg["reason"] == "eof",
            _ => {}
        }
    }

    fn position(&self, video_id: &str, done: bool) -> Option<Position> {
        // At the end of the file time-pos stops a little short of the duration.
        let secs = if self.eof {
            self.duration.or(self.time_pos)
        } else {
            self.time_pos
        }?;
        Some(Position {
            video_id: video_id.to_string(),
            secs,
            duration: self.duration,
            done,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> Tracker {
        let mut t = Tracker::default();
        for line in lines {
            t.handle(line);
        }
        t
    }

    #[test]
    fn quitting_partway_reports_the_last_position() {
        let t = feed(&[
            r#"{"data":null,"request_id":0,"error":"success"}"#,
            r#"{"event":"property-change","id":2,"name":"duration","data":600.5}"#,
            r#"{"event":"property-change","id":1,"name":"time-pos","data":41.2}"#,
            r#"{"event":"property-change","id":1,"name":"time-pos","data":42.0}"#,
            r#"{"event":"end-file","reason":"quit","playlist_entry_id":1}"#,
            r#"{"event":"property-change","id":1,"name":"time-pos"}"#,
        ]);
        let p = t.position("v", true).unwrap();
        assert_eq!((p.secs, p.duration, p.done), (42.0, Some(600.5), true));
    }

    #[test]
    fn end_of_file_reports_the_duration() {
        let t = feed(&[
            r#"{"event":"property-change","id":2,"name":"duration","data":600.5}"#,
            r#"{"event":"property-change","id":1,"name":"time-pos","data":599.9}"#,
            r#"{"event":"end-file","reason":"eof","playlist_entry_id":1}"#,
        ]);
        assert_eq!(t.position("v", true).unwrap().secs, 600.5);
    }

    /// The real watcher against a fake mpv: connects, asks for the
    /// properties, and reports the final position when mpv goes away.
    #[cfg(unix)]
    #[test]
    fn watches_mpv_over_a_socket() {
        use std::os::unix::net::UnixListener;

        let endpoint = endpoint();
        let listener = UnixListener::bind(&endpoint).unwrap();
        let queue = PositionQueue::default();
        watch(endpoint.clone(), "abc".into(), queue.clone());

        let (stream, _) = listener.accept().unwrap();
        let mut commands = BufReader::new(stream.try_clone().unwrap()).lines();
        assert!(commands.next().unwrap().unwrap().contains("time-pos"));
        assert!(commands.next().unwrap().unwrap().contains("duration"));
        let mut mpv = stream;
        mpv.write_all(
            concat!(
                r#"{"event":"property-change","id":2,"name":"duration","data":300.0}"#,
                "\n",
                r#"{"event":"property-change","id":1,"name":"time-pos","data":123.5}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();
        drop(mpv);
        drop(commands);

        let deadline = Instant::now() + Duration::from_secs(5);
        let reported = loop {
            let positions = queue.take();
            if !positions.is_empty() || Instant::now() > deadline {
                break positions;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            reported,
            vec![Position {
                video_id: "abc".into(),
                secs: 123.5,
                duration: Some(300.0),
                done: true,
            }]
        );
        let _ = std::fs::remove_file(&endpoint);
    }

    #[test]
    fn nothing_played_reports_nothing() {
        let t = feed(&[r#"{"event":"end-file","reason":"error"}"#, "not json"]);
        assert_eq!(t.position("v", true), None);
    }
}
