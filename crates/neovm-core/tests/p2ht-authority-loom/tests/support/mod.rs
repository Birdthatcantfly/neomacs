//! Exploration receipts use std atomics, never synchronization in the model.
#![allow(dead_code)]

pub fn check(threads: usize, body: impl Fn() + Send + Sync + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.max_threads = threads;
    builder.max_branches = 512;
    builder.max_permutations = None;
    builder.max_duration = None;
    builder.checkpoint_file = None;
    builder.location = true;
    // Preserve LOOM_MAX_PREEMPTIONS from Builder::new(). No time/iteration cap.
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
