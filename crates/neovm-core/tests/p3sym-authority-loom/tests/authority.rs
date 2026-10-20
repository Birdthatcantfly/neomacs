mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::thread;
use model::{Authority, COUNT_MASK, Cell, Latch, WRITER};

#[test]
fn symbol_cell_copy_vs_writer() {
    support::check(3, || {
        let shared = Arc::new((Authority::new(), Cell::new()));
        let reader_shared = Arc::clone(&shared);
        let reader = thread::spawn(move || {
            // Finite attempts model a wrapper returning/retrying outside its
            // loans. Contention may refuse; successful reads must be coherent.
            for _ in 0..2 {
                if let Some(_lease) = reader_shared.0.read() {
                    let (value, version) = reader_shared.1.copy();
                    assert_eq!(value, version * 10, "torn symbol cell");
                }
            }
        });
        let writer_shared = Arc::clone(&shared);
        let writer = thread::spawn(move || {
            if let Some(_lease) = writer_shared.0.write() {
                writer_shared.1.set(2);
            }
        });
        // A second reader checks a release sequence across reader RMWs.
        if let Some(_lease) = shared.0.read() {
            let (value, version) = shared.1.copy();
            assert_eq!(value, version * 10);
        }
        reader.join().unwrap();
        writer.join().unwrap();
        shared.0.assert_quiescent();
    });
}

#[test]
fn readers_overlap_and_writer_drains() {
    support::check(3, || {
        let authority = Arc::new(Authority::new());
        let first = authority.read().unwrap();
        let entered = Arc::new(Latch::default());
        let release = Arc::new(Latch::default());
        let (a, e, r) = (
            Arc::clone(&authority),
            Arc::clone(&entered),
            Arc::clone(&release),
        );
        let second = thread::spawn(move || {
            let lease = a.read().unwrap();
            e.set();
            r.wait();
            drop(lease);
        });
        entered.wait();
        assert_eq!(authority.word.load(Ordering::Acquire), 2);
        assert!(authority.write().is_none());
        drop(first);
        assert!(authority.write().is_none());
        release.set();
        second.join().unwrap();
        drop(
            authority
                .write()
                .expect("writer succeeds after reader drain"),
        );
        authority.assert_quiescent();
    });
}

#[test]
fn reader_retries_after_writer_release() {
    support::exhaustive(2, || {
        let shared = Arc::new((Authority::new(), Cell::new()));
        let lease = shared.0.write().unwrap();
        let refused = Arc::new(Latch::default());
        let retry = Arc::new(Latch::default());
        let (s, f, r) = (
            Arc::clone(&shared),
            Arc::clone(&refused),
            Arc::clone(&retry),
        );
        let reader = thread::spawn(move || {
            assert!(s.0.read().is_none(), "reader did not refuse WRITER");
            f.set();
            r.wait();
            let _lease = s.0.read().expect("retry after writer completed");
            assert_eq!(s.1.copy(), (20, 2));
        });
        refused.wait();
        shared.1.set(2);
        drop(lease);
        retry.set();
        reader.join().unwrap();
        shared.0.assert_quiescent();
    });
}

#[test]
fn reader_not_retrying_across_writer_is_rejected() {
    support::negative(2, "reader admitted across WRITER without retry", || {
        let authority = Arc::new(Authority::new());
        let _writer = authority.write().unwrap();
        let a = Arc::clone(&authority);
        thread::spawn(move || {
            // Broken variant uses an unconditional reader RMW instead of
            // refusing/retrying WRITER. Undo the count before auditing the
            // witness so unwinding cannot create a second guard panic.
            let before = a.word.fetch_add(1, Ordering::AcqRel);
            a.word.fetch_sub(1, Ordering::Release);
            assert!(
                before & WRITER == 0,
                "reader admitted across WRITER without retry"
            );
        })
        .join()
        .unwrap();
    });
}

#[test]
fn writer_bypassing_drain_races_checked_symbol_cell() {
    support::negative(
        2,
        "Causality violation: Concurrent read and write accesses to `UnsafeCell`.",
        || {
            let shared = Arc::new((Authority::new(), Cell::new()));
            let s = Arc::clone(&shared);
            let read_done = Arc::new(AtomicBool::new(false));
            let done = Arc::clone(&read_done);
            let reader = thread::spawn(move || {
                let _lease = s.0.read().unwrap();
                let _ = s.1.copy();
                done.store(true, Ordering::Relaxed);
            });
            // Relaxed sequencing deliberately provides no happens-before for
            // ordinary cell data; the broken writer skips authority entirely.
            if read_done.load(Ordering::Relaxed) {
                shared.1.set(2);
            }
            reader.join().unwrap();
        },
    );
}

#[test]
fn reader_count_saturation_refuses_without_wrapping() {
    support::exhaustive(1, || {
        let authority = Authority::new();
        authority.word.store(COUNT_MASK, Ordering::Relaxed);
        assert!(authority.read().is_none());
        assert!(authority.write().is_none());
        assert_eq!(authority.word.load(Ordering::Relaxed), COUNT_MASK);
    });
}
