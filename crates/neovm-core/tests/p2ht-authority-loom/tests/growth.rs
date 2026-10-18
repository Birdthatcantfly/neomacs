//! Growth under WRITER with the reader drain (design §3.3, §4 Rev R3): the
//! reallocated old backing is freed only after zero readers remain in
//! flight, and the negative control for growing without the drain.
#![forbid(unsafe_code)]

mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::thread;

use model::{Table, WriterLease, wait_for};

/// A reader that entered on backing generation G copies its entry and
/// asserts it is not walking a freed backing. The writer grows only after
/// its drain CAS, which can only succeed once that reader has left, so the
/// assert holds in every explored execution.
#[test]
fn growth_frees_old_backing_only_after_drain() {
    support::check(3, || {
        let table = Arc::new(Table::new());
        let r_in = Arc::new(AtomicBool::new(false));
        let w_attempting = Arc::new(AtomicBool::new(false));

        let (t, r, w) = (
            Arc::clone(&table),
            Arc::clone(&r_in),
            Arc::clone(&w_attempting),
        );
        let reader = thread::spawn(move || {
            let lease = model::ReaderLease::enter(&t);
            let backing = t.live_gen.load(Ordering::Acquire);
            r.store(true, Ordering::Release);
            wait_for(&w);
            // The backing this reader walks must not have been freed.
            assert!(
                backing > t.freed_gen.load(Ordering::Acquire),
                "reader walked a freed backing"
            );
            let entry = lease.copy_entry();
            assert!(Table::entry_consistent(entry));
            drop(lease);
        });

        let (t, r, w) = (
            Arc::clone(&table),
            Arc::clone(&r_in),
            Arc::clone(&w_attempting),
        );
        let writer = thread::spawn(move || {
            wait_for(&r);
            w.store(true, Ordering::Release);
            let lease = WriterLease::enter(&t);
            lease.grow();
            lease.mutate();
            drop(lease);
        });

        reader.join().unwrap();
        writer.join().unwrap();
        assert_eq!(table.live_gen.load(Ordering::Acquire), 2);
        assert_eq!(table.freed_gen.load(Ordering::Acquire), 1);
    });
}

/// NEGATIVE control — a writer that grows (freeing the old backing) without
/// taking the word or draining. The in-flight reader is still walking the
/// generation the allocator just freed.
#[test]
fn growth_without_drain_frees_live_backing() {
    const EXPECTED: &str = "reader walked a freed backing";
    support::negative(3, EXPECTED, || {
        let table = Arc::new(Table::new());
        let r_in = Arc::new(AtomicBool::new(false));

        let (t, r) = (Arc::clone(&table), Arc::clone(&r_in));
        let reader = thread::spawn(move || {
            let lease = model::ReaderLease::enter(&t);
            let backing = t.live_gen.load(Ordering::Acquire);
            r.store(true, Ordering::Release);
            assert!(
                backing > t.freed_gen.load(Ordering::Acquire),
                "{}",
                EXPECTED
            );
            let _ = lease.copy_entry();
            drop(lease);
        });

        let (t, r) = (Arc::clone(&table), Arc::clone(&r_in));
        let writer = thread::spawn(move || {
            wait_for(&r);
            // Buggy growth: no WRITER, no drain — the old backing is freed
            // under the reader's feet.
            t.writer_grow();
            t.writer_mutate();
        });

        reader.join().unwrap();
        writer.join().unwrap();
    });
}
