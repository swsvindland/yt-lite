//! Process memory readout for verifying the memory budget.
//!
//! `physical` is the resident set; on Windows that is the working set, which is
//! what Task Manager's "Memory" column approximates (it shows the *private*
//! working set, so it reads somewhat lower).

#[derive(Clone, Copy, Debug, Default)]
pub struct MemUsage {
    pub physical: usize,
}

pub fn current() -> Option<MemUsage> {
    memory_stats::memory_stats().map(|m| MemUsage {
        physical: m.physical_mem,
    })
}

pub fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

pub fn log_now(context: &str) {
    if let Some(m) = current() {
        log::info!("memory [{context}]: rss {:.1} MB", mb(m.physical));
    }
}
