//! On-disk thumbnail cache (JPEG bytes as downloaded), pruned least-recently-used.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use crate::net::Http;
use crate::youtube::thumbnail_url;

/// Blocking: returns the JPEG bytes from the disk cache, downloading on a miss.
pub fn fetch(http: &Http, dir: &Path, video_id: &str) -> Result<Vec<u8>> {
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
    Ok(bytes)
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
            meta.is_file().then(|| {
                (
                    e.path(),
                    meta.len(),
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                )
            })
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

    #[test]
    fn disk_prune_removes_oldest_first() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let files = vec![
            (PathBuf::from("new"), 40, t0 + Duration::from_secs(30)),
            (PathBuf::from("old"), 40, t0),
            (PathBuf::from("mid"), 40, t0 + Duration::from_secs(10)),
        ];
        assert_eq!(
            select_for_deletion(files.clone(), 120),
            Vec::<PathBuf>::new()
        );
        assert_eq!(
            select_for_deletion(files.clone(), 80),
            vec![PathBuf::from("old")]
        );
        assert_eq!(
            select_for_deletion(files, 10),
            vec![
                PathBuf::from("old"),
                PathBuf::from("mid"),
                PathBuf::from("new")
            ]
        );
    }
}
