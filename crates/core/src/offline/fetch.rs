//! Downloads one stream to a file in bounded range requests, resuming from
//! whatever the file already holds.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::net::{Http, HttpStatus};

/// googlevideo throttles open-ended requests to about real time, and
/// requests for more than about 10 MB too; bounded ranges up to this size
/// run at full speed.
pub const RANGE: u64 = 10 * 1024 * 1024;

/// Waits between attempts after network errors; then it gives up.
#[cfg(not(test))]
const BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
    Duration::from_secs(20),
];
#[cfg(test)]
const BACKOFF: [Duration; 4] = [Duration::from_millis(10); 4];

#[derive(Debug)]
pub enum FetchError {
    /// `stop` said so.
    Stopped,
    /// The server refused the URL (expired after about 6 hours, or bound to
    /// another IP address): resolve the video again.
    Refused(u16),
    /// No connection, or it kept failing.
    Network(String),
    /// The disk (full, removed).
    Io(std::io::Error),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Stopped => f.write_str("stopped"),
            FetchError::Refused(status) => {
                write!(f, "the server refused the download (HTTP {status})")
            }
            FetchError::Network(e) => write!(f, "network error: {e}"),
            FetchError::Io(e) => write!(f, "couldn't save the download: {e}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Fetches `url` into `path` until the file holds `len` bytes. `progress`
/// gets the file's length as it grows; `stop` is polled between reads.
pub fn fetch(
    http: &Http,
    url: &str,
    user_agent: &str,
    path: &Path,
    len: u64,
    stop: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), FetchError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(FetchError::Io)?;
    let mut have = file.metadata().map_err(FetchError::Io)?.len();
    if have > len {
        // Not the stream it started as.
        file.set_len(0).map_err(FetchError::Io)?;
        have = 0;
    }
    progress(have);
    let mut failures = 0;
    while have < len {
        if stop() {
            return Err(FetchError::Stopped);
        }
        let end = (have + RANGE).min(len) - 1;
        let result = fetch_range(http, url, user_agent, &mut file, have, end, stop, progress);
        let now = file.metadata().map_err(FetchError::Io)?.len();
        if now > have {
            failures = 0;
        }
        have = now;
        match result {
            Ok(()) => {}
            Err(RangeError::Stopped) => return Err(FetchError::Stopped),
            Err(RangeError::Io(e)) => return Err(FetchError::Io(e)),
            // Server trouble: try again like a dropped connection.
            Err(RangeError::Status(s)) if s >= 500 || s == 429 => {
                failures += 1;
                retry_after(failures, &format!("HTTP {s}"), stop)?;
            }
            Err(RangeError::Status(s)) => return Err(FetchError::Refused(s)),
            Err(RangeError::Network(e)) => {
                failures += 1;
                retry_after(failures, &e, stop)?;
            }
        }
    }
    file.sync_data().map_err(FetchError::Io)?;
    Ok(())
}

/// Sleeps before the next attempt, or gives up after the last one.
fn retry_after(failures: usize, error: &str, stop: &dyn Fn() -> bool) -> Result<(), FetchError> {
    let Some(&wait) = BACKOFF.get(failures - 1) else {
        return Err(FetchError::Network(error.to_string()));
    };
    log::debug!("download failed ({error}), retrying in {wait:?}");
    let until = std::time::Instant::now() + wait;
    while std::time::Instant::now() < until {
        if stop() {
            return Err(FetchError::Stopped);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

enum RangeError {
    Stopped,
    Status(u16),
    Network(String),
    Io(std::io::Error),
}

/// Appends bytes `start..=end` to `file`. Whatever arrived before an error
/// stays in the file (the caller resumes from its length).
#[allow(clippy::too_many_arguments)]
fn fetch_range(
    http: &Http,
    url: &str,
    user_agent: &str,
    file: &mut File,
    start: u64,
    end: u64,
    stop: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), RangeError> {
    let mut body = http.get_range(url, user_agent, start, end).map_err(|e| {
        match e.downcast_ref::<HttpStatus>() {
            Some(s) => RangeError::Status(s.0),
            None => RangeError::Network(format!("{e:#}")),
        }
    })?;
    let want = end - start + 1;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0u64;
    while got < want {
        if stop() {
            out.flush().map_err(RangeError::Io)?;
            return Err(RangeError::Stopped);
        }
        let room = buf.len().min((want - got) as usize);
        let n = match body.read(&mut buf[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                out.flush().map_err(RangeError::Io)?;
                return Err(RangeError::Network(e.to_string()));
            }
        };
        out.write_all(&buf[..n]).map_err(RangeError::Io)?;
        got += n as u64;
        progress(start + got);
    }
    out.flush().map_err(RangeError::Io)?;
    if got < want {
        return Err(RangeError::Network(format!(
            "connection closed after {got} of {want} bytes"
        )));
    }
    Ok(())
}
