//! Immutable process diagnostics, with no Lisp roots or editor policy.
//!
//! The first accepted frame captures the Unicode layout-stats path. OnceLock
//! publishes its immutable Rust string to all mutators; each accepted frame
//! still opens/appends the file exactly as before. No TLS or engine field is
//! added, and changing the environment later does not redirect the stream.

use std::sync::OnceLock;

#[inline]
pub(super) fn layout_stats_path() -> Option<&'static str> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| {
        #[cfg(test)]
        ENV_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::var("NEOMACS_LAYOUT_STATS_FILE").ok()
    })
    .as_deref()
    .filter(|path| !path.is_empty())
}

#[cfg(test)]
static ENV_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
#[path = "tests/diagnostic_knobs_test.rs"]
mod tests;
