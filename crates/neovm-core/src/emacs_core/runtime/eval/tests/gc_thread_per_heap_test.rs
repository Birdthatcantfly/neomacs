//! Each heap has its own collector thread (`tagged/gc/gc_thread.rs`
//! `COLLECTORS`): Worlds on one OS thread no longer queue behind each
//! other's concurrent marks, and a World's collector retires with it.

use super::Context;
use crate::emacs_core::print::print_value;
use std::sync::mpsc;
use std::time::Duration;

const KEEP: &str = "(setq tph-keep
  (let ((l nil) (i 0))
    (while (< i 2000) (setq l (cons (cons i (make-string 3 ?a)) l) i (1+ i)))
    l))";
const KEEP_SUM: &str =
    "(let ((s 0) (l tph-keep)) (while l (setq s (+ s (car (car l))) l (cdr l))) s)";
const CHURN: &str = "(let ((i 0)) (while (< i 2000) (make-list 8 i) (setq i (1+ i))))";

fn eval(ev: &mut Context, src: &str) -> String {
    ev.setup_thread_locals();
    match ev.eval_str(src) {
        Ok(value) => print_value(&value),
        Err(error) => format!("ERR {error:?}"),
    }
}

/// Allocate and collect until `ev` has a concurrent mark in flight, and
/// leave it running. A generational heap runs up to
/// `NEOVM_GC_MAJOR_MAX_MINORS` (64) synchronous minors, each with its
/// deferred sweep, before the concurrent major.
fn launch_concurrent_mark(ev: &mut Context) {
    for _ in 0..4096 {
        eval(ev, CHURN);
        ev.gc_collect_from_current_roots_impl(false);
        if ev.tagged_heap.concurrent_mark_running() {
            return;
        }
    }
    panic!("the World never launched a concurrent mark");
}

/// Run `body` on its own thread, failing (rather than hanging the test
/// binary) if it does not finish in time.
fn within_timeout(body: impl FnOnce() + Send + 'static) {
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        body();
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => worker.join().expect("the body panicked after finishing"),
        // The body panicked: surface its message.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(worker.join().expect_err("the body did not finish"))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("a World's collection is stuck behind another World's collector")
        }
    }
}

#[test]
fn two_worlds_on_one_thread_collect_while_both_mark() {
    within_timeout(|| {
        #[cfg(debug_assertions)]
        crate::tagged::gc::set_verify_marked_objects_for_test(true);
        let mut worlds = [Context::new(), Context::new()];
        for ev in &mut worlds {
            let kept = eval(ev, KEEP);
            assert!(!kept.starts_with("ERR"), "{kept}");
        }
        for round in 0..4 {
            launch_concurrent_mark(&mut worlds[0]);
            launch_concurrent_mark(&mut worlds[1]);
            // Each World collects synchronously while the other's mark is
            // in flight; the order alternates by round.
            let (a, b) = if round % 2 == 0 { (0, 1) } else { (1, 0) };
            worlds[a].setup_thread_locals();
            worlds[a].gc_collect_from_current_roots_impl(true);
            assert!(worlds[b].tagged_heap.concurrent_mark_running());
            launch_concurrent_mark(&mut worlds[a]);
            worlds[b].setup_thread_locals();
            worlds[b].gc_collect_from_current_roots_impl(true);
            assert!(worlds[a].tagged_heap.concurrent_mark_running());
            for ev in &mut worlds {
                assert_eq!(eval(ev, KEEP_SUM), "1999000", "round {round}");
            }
        }
        for ev in &mut worlds {
            ev.setup_thread_locals();
            ev.finish_concurrent_mark()
                .expect("finish the concurrent mark");
            ev.gc_collect_from_current_roots_impl(true);
            assert_eq!(eval(ev, KEEP_SUM), "1999000");
        }
        #[cfg(debug_assertions)]
        crate::tagged::gc::set_verify_marked_objects_for_test(false);
    });
}

/// How many of this process's threads are collectors. Nextest runs each
/// test in its own process, so only the test's own heaps count.
#[cfg(target_os = "linux")]
fn collector_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("read /proc/self/task")
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .filter(|comm| comm.trim_end() == crate::tagged::gc::COLLECTOR_THREAD_NAME)
        .count()
}

/// Wait until `expected` collectors are listed. A joined thread has finished
/// running, but the kernel unlists it a moment after `pthread_join` returns.
#[cfg(target_os = "linux")]
fn await_collector_threads(expected: usize, limit: Duration) {
    let start = std::time::Instant::now();
    while collector_threads() != expected {
        assert!(
            start.elapsed() < limit,
            "{} collector threads, expected {expected}",
            collector_threads()
        );
        std::thread::sleep(Duration::from_micros(100));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn dropping_a_world_joins_its_collector() {
    let before = collector_threads();
    let mut ev = Context::new();
    ev.gc_collect_from_current_roots_impl(true);
    let heap = ev.tagged_heap.heap_identity();
    assert!(crate::tagged::gc::collector_is_registered(heap));
    assert_eq!(collector_threads(), before + 1, "one collector per heap");
    drop(ev);
    assert!(!crate::tagged::gc::collector_is_registered(heap));
    await_collector_threads(before, Duration::from_secs(1));
}

#[cfg(target_os = "linux")]
#[test]
fn a_world_dropped_mid_mark_detaches_a_collector_that_then_exits() {
    within_timeout(|| {
        let before = collector_threads();
        let mut ev = Context::new();
        launch_concurrent_mark(&mut ev);
        let heap = ev.tagged_heap.heap_identity();
        assert!(crate::tagged::gc::collector_is_registered(heap));
        assert_eq!(collector_threads(), before + 1, "one collector per heap");
        // Drop abandons the running job without waiting for it.
        drop(ev);
        assert!(!crate::tagged::gc::collector_is_registered(heap));
        await_collector_threads(before, Duration::from_secs(30));
    });
}
