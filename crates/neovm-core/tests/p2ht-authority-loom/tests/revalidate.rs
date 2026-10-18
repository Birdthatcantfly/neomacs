//! `switch_epoch` revalidation across a modeled user callback (design §2.1
//! reader protocol, §3.1): copy candidates under the read count, drop the
//! count, run the callback, re-read the epoch, retry on change — and the
//! negative control for skipping the recheck.
#![forbid(unsafe_code)]

mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::thread;

use model::{K1, K2, Table, V1, V2, wait_for};

/// One writer mutation races a reader that follows the full protocol. The
/// accepted pair is whatever a stable epoch validated, never a torn one, and
/// the retry path is exercised while the writer lands between validation
/// rounds.
#[test]
fn revalidation_retries_until_epoch_stable() {
    support::check(3, || {
        let table = Arc::new(Table::new());
        let started = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(AtomicUsize::new((K1, V1).0));

        let (t, s) = (Arc::clone(&table), Arc::clone(&started));
        let writer = thread::spawn(move || {
            wait_for(&s);
            let lease = model::WriterLease::enter(&t);
            lease.mutate();
            drop(lease);
        });

        let (t, s, out) = (
            Arc::clone(&table),
            Arc::clone(&started),
            Arc::clone(&accepted),
        );
        let reader = thread::spawn(move || {
            s.store(true, Ordering::Release);
            let mut rounds = 0;
            let entry = loop {
                rounds += 1;
                assert!(rounds <= 8, "revalidation did not converge");
                let before = t.epoch.load(Ordering::Acquire);
                let lease = model::ReaderLease::enter(&t);
                let entry = lease.copy_entry();
                drop(lease);
                // Modeled user callback: no authority held across it.
                loom::thread::yield_now();
                loom::thread::yield_now();
                let after = t.epoch.load(Ordering::Acquire);
                if before == after {
                    break entry;
                }
            };
            assert!(Table::entry_consistent(entry));
            out.store(entry.0, Ordering::Release);
        });

        writer.join().unwrap();
        reader.join().unwrap();
        let key = accepted.load(Ordering::Acquire);
        assert!(key == K1 || key == K2, "accepted a torn or alien entry");
    });
}

/// Successive mutation windows still converge. The reader runs three fixed
/// validation rounds; each of the first two opens a callback window (drops
/// the read count, releases one writer mutation, races the post-callback
/// epoch load against it) and then blocks on the writer's window-completion
/// latch, so each mutation races exactly that one load — Loom explores both
/// orderings per window while the two windows stay independent instead of
/// multiplying. Round three runs with the writer parked at completion, so
/// its epoch is provably stable: the protocol converges once mutations stop.
/// The data-dependent retry loop against a free-running writer is
/// `revalidation_retries_until_epoch_stable` above; a second free writer
/// section there squares the interleaving space past the 512-branch budget.
#[test]
fn revalidation_converges_under_repeated_mutation() {
    support::check(3, || {
        let table = Arc::new(Table::new());
        // (open, done) per window; every latch is fired unconditionally by
        // construction, so neither actor can hang.
        let window_one = Arc::new((model::Latch::new(), model::Latch::new()));
        let window_two = Arc::new((model::Latch::new(), model::Latch::new()));

        let (t, w1, w2) = (
            Arc::clone(&table),
            Arc::clone(&window_one),
            Arc::clone(&window_two),
        );
        let writer = thread::spawn(move || {
            w1.0.wait();
            let lease = model::WriterLease::enter(&t);
            lease.mutate();
            drop(lease);
            w1.1.set();
            w2.0.wait();
            let lease = model::WriterLease::enter(&t);
            lease.mutate();
            drop(lease);
            w2.1.set();
        });

        let (t, w1, w2) = (
            Arc::clone(&table),
            Arc::clone(&window_one),
            Arc::clone(&window_two),
        );
        let reader = thread::spawn(move || {
            for round in 0..3 {
                let before = t.epoch.load(Ordering::Acquire);
                let lease = model::ReaderLease::enter(&t);
                let entry = lease.copy_entry();
                drop(lease);
                match round {
                    0 => {
                        // Window one: release the mutation, then race the
                        // post-callback epoch load against it.
                        w1.0.set();
                        t.epoch.load(Ordering::Acquire);
                        w1.1.wait();
                    }
                    1 => {
                        w2.0.set();
                        t.epoch.load(Ordering::Acquire);
                        w2.1.wait();
                    }
                    _ => {
                        // Writer parked at window two's completion latch.
                        let after = t.epoch.load(Ordering::Acquire);
                        assert_eq!(after, before, "epoch moved with no writer running");
                    }
                }
                assert!(
                    Table::entry_consistent(entry),
                    "round {} accepted a torn entry",
                    round
                );
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
        assert_eq!(
            (
                table.key_word.load(Ordering::Acquire),
                table.val_word.load(Ordering::Acquire)
            ),
            (K2, V2)
        );
        assert_eq!(table.authority.load(Ordering::Acquire), 0);
    });
}

/// NEGATIVE control — the reader accepts the entry copied before its
/// callback without re-reading the epoch. A removal that lands inside the
/// callback window leaves the reader asserting a stale answer.
#[test]
fn reader_skipping_revalidation_accepts_stale_entry() {
    const EXPECTED: &str = "stale answer accepted after callback";
    support::negative(3, EXPECTED, || {
        let table = Arc::new(Table::new());
        let started = Arc::new(AtomicBool::new(false));

        let (t, s) = (Arc::clone(&table), Arc::clone(&started));
        let writer = thread::spawn(move || {
            wait_for(&s);
            let lease = model::WriterLease::enter(&t);
            lease.mutate();
            drop(lease);
            let lease = model::WriterLease::enter(&t);
            t.writer_remove();
            drop(lease);
        });

        let (t, s) = (Arc::clone(&table), Arc::clone(&started));
        let reader = thread::spawn(move || {
            s.store(true, Ordering::Release);
            let lease = model::ReaderLease::enter(&t);
            let entry = lease.copy_entry();
            drop(lease);
            // Modeled callback, no revalidation afterwards.
            loom::thread::yield_now();
            loom::thread::yield_now();
            let lease = model::ReaderLease::enter(&t);
            let current = lease.copy_entry();
            drop(lease);
            if entry != current {
                panic!("{}", EXPECTED);
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    });
}
