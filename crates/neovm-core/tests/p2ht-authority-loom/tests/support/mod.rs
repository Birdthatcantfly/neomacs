//! Exploration receipts use std atomics, never synchronization in the model.
#![allow(dead_code)]

pub fn check(threads: usize, body: impl Fn() + Send + Sync + 'static) {
    check_impl(threads, None, body)
}

/// Preemption-bounded exploration for the B4-sized schedules: four actors
/// plus the main thread is beyond exhaustive reach (measured >30 min), so cap
/// the preemption depth the way LOOM_MAX_PREEMPTIONS would. The flag gates
/// inside those tests force the interesting schedules deterministically; the
/// bound only trims redundant preemption permutations of those schedules.
pub fn check_bounded(threads: usize, bound: usize, body: impl Fn() + Send + Sync + 'static) {
    check_impl(threads, Some(bound), body)
}

fn check_impl(threads: usize, bound: Option<usize>, body: impl Fn() + Send + Sync + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.max_threads = threads;
    builder.max_branches = 512;
    builder.max_permutations = None;
    builder.max_duration = None;
    builder.checkpoint_file = None;
    builder.location = true;
    builder.preemption_bound = bound;
    // check() preserves LOOM_MAX_PREEMPTIONS from Builder::new(). No time/iteration cap.
    struct Receipt(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Receipt {
        fn drop(&mut self) {
            eprintln!(
                "LOOM_ITERATIONS {} {}",
                std::thread::current().name().unwrap_or("unnamed"),
                self.0.load(std::sync::atomic::Ordering::Relaxed)
            );
        }
    }
    let receipt = Receipt(std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let count = std::sync::Arc::clone(&receipt.0);
    builder.check(move || {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        body();
    });
}

pub fn negative(threads: usize, expected: &'static str, body: impl Fn() + Send + Sync + 'static) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(threads, body)));
    let failure = result.expect_err("negative control did not find its required witness");
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .expect("negative control produced a non-string panic");
    assert_eq!(
        message, expected,
        "unrelated panic cannot satisfy a control"
    );
    eprintln!(
        "LOOM_WITNESS {} {}",
        std::thread::current().name().unwrap_or("unnamed"),
        expected
    );
}
