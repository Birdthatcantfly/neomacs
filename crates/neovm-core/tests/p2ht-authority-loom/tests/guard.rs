//! Rev R2 user-test nesting count: two owners inside tests on one table, a
//! third owner's mutation must signal; RAII unwind; and the lost-restore
//! negative control for GNU's bool flag (`tmp/peer-p2ht/design.md` §3.1).
#![forbid(unsafe_code)]

mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::thread;

use model::{GuardHold, Table, wait_for};

/// Two owners nest inside user tests on one shared table (depth 2) and a
/// THIRD owner's mutation signals "hash table test modifies table" while
/// either holds. Both holders exit only after the third owner's attempt, so
/// the check happens at depth 2 in every explored execution. The THIRD owner
/// is the model's main thread: two spawned actors on blocking latches keep
/// Loom's exploration tractable (a third spawned actor multiplied the
/// interleaving space past the 512-branch budget for minutes on end).
#[test]
fn nested_tests_two_owners_third_mutation_signals() {
    support::check(3, || {
        let table = Arc::new(Table::new());
        let a_in = Arc::new(model::Latch::new());
        let b_in = Arc::new(model::Latch::new());
        let release = Arc::new(model::Latch::new());

        let (t, a, r) = (Arc::clone(&table), Arc::clone(&a_in), Arc::clone(&release));
        let owner_a = thread::spawn(move || {
            let hold = GuardHold::enter(&t);
            a.set();
            r.wait();
            drop(hold);
        });

        let (t, a, b, r) = (
            Arc::clone(&table),
            Arc::clone(&a_in),
            Arc::clone(&b_in),
            Arc::clone(&release),
        );
        let owner_b = thread::spawn(move || {
            a.wait();
            let hold = GuardHold::enter(&t);
            b.set();
            r.wait();
            drop(hold);
        });

        // Third owner on the main thread: B's latch release happens after B's
        // enter, and A is still held (it only drops after `release`), so the
        // check below runs at depth 2 in every explored execution.
        b_in.wait();
        assert_eq!(
            table.guard_depth.load(Ordering::Acquire),
            2,
            "nesting count did not reach two"
        );
        let attempt = table.check_mutable();
        assert!(
            attempt.is_err(),
            "mutation inside active user test was not signaled"
        );
        assert_eq!(
            attempt.unwrap_err(),
            "hash table test modifies table",
            "wrong signal text"
        );
        release.set();

        owner_a.join().unwrap();
        owner_b.join().unwrap();
        assert_eq!(table.guard_depth.load(Ordering::Acquire), 0);
    });
}

/// The RAII hold releases its nesting count on early return, the model of
/// unwinding out of a user test function (GNU's specpdl record_unwind).
/// Three unique threads: the model main plus one actor per phase.
#[test]
fn guard_hold_unwinds_to_zero() {
    support::check(3, || {
        let table = Arc::new(Table::new());
        let t = Arc::clone(&table);

        let early = thread::spawn(move || {
            let hold = GuardHold::enter(&t);
            let _keep = hold;
            if t.guard_depth.load(Ordering::Acquire) == 1 {
                // Early return drops the hold on the way out.
                return;
            }
            unreachable!();
        });
        early.join().unwrap();
        assert_eq!(table.guard_depth.load(Ordering::Acquire), 0);
        // Nested holds unwind inside-out.
        let t = Arc::clone(&table);
        let nested = thread::spawn(move || {
            let outer = GuardHold::enter(&t);
            let result = (|| {
                let _inner = GuardHold::enter(&t);
                if t.guard_depth.load(Ordering::Acquire) == 2 {
                    return 1;
                }
                0
            })();
            assert_eq!(result, 1);
            assert_eq!(t.guard_depth.load(Ordering::Acquire), 1);
            drop(outer);
        });
        nested.join().unwrap();
        assert_eq!(table.guard_depth.load(Ordering::Acquire), 0);
    });
}

/// A single owner's own mutation inside its test still signals — the serial
/// GNU rule the 42-case matrix pins, unchanged under the count.
#[test]
fn own_mutation_inside_own_test_signals() {
    support::check(2, || {
        let table = Arc::new(Table::new());
        let t = Arc::clone(&table);
        let owner = thread::spawn(move || {
            let hold = GuardHold::enter(&t);
            assert!(t.check_mutable().is_err());
            drop(hold);
            assert!(t.check_mutable().is_ok());
        });
        owner.join().unwrap();
    });
}

/// NEGATIVE control — GNU's bool flag with its unconditional restore
/// (`restore_mutability`, fns.c:4713-4716). Owner A exits its test and
/// stores `true` while owner B is still inside B's test (B entered while the
/// flag was already false, so like GNU's nested call it recorded nothing);
/// owner C's mutation then finds the flag true and is NOT signaled.
#[test]
fn bool_guard_lost_restore_misses_signal() {
    const EXPECTED: &str = "mutation inside active user test was not signaled";
    support::negative(4, EXPECTED, || {
        struct BuggyTable {
            mutable: AtomicBool,
        }
        // GNU shape: only an enter that observes `true` records a restore;
        // the restore stores `true` unconditionally.
        fn buggy_enter(table: &BuggyTable) -> bool {
            if table.mutable.load(Ordering::Relaxed) {
                table.mutable.store(false, Ordering::Relaxed);
                true
            } else {
                false
            }
        }
        fn buggy_restore(table: &BuggyTable) {
            table.mutable.store(true, Ordering::Relaxed);
        }

        let table = Arc::new(BuggyTable {
            mutable: AtomicBool::new(true),
        });
        let a_in = Arc::new(AtomicBool::new(false));
        let b_in = Arc::new(AtomicBool::new(false));
        let a_restored = Arc::new(AtomicBool::new(false));
        let c_done = Arc::new(AtomicBool::new(false));

        let (t, a, b, out) = (
            Arc::clone(&table),
            Arc::clone(&a_in),
            Arc::clone(&b_in),
            Arc::clone(&a_restored),
        );
        let owner_a = thread::spawn(move || {
            assert!(buggy_enter(&t), "first enter must record the restore");
            a.store(true, Ordering::Release);
            wait_for(&b);
            buggy_restore(&t);
            out.store(true, Ordering::Release);
        });

        let (t, a, b, c) = (
            Arc::clone(&table),
            Arc::clone(&a_in),
            Arc::clone(&b_in),
            Arc::clone(&c_done),
        );
        let owner_b = thread::spawn(move || {
            wait_for(&a);
            // GNU's nested shape: the flag is already false, so this enter
            // records nothing and its exit restores nothing.
            assert!(!buggy_enter(&t), "nested enter records nothing");
            b.store(true, Ordering::Release);
            // B stays inside its test across C's mutation attempt.
            wait_for(&c);
        });

        let (t, b, out, c) = (
            Arc::clone(&table),
            Arc::clone(&b_in),
            Arc::clone(&a_restored),
            Arc::clone(&c_done),
        );
        let owner_c = thread::spawn(move || {
            wait_for(&b);
            wait_for(&out);
            if !t.mutable.load(Ordering::Relaxed) {
                // Correct behavior: the flag would still guard B's test.
                c.store(true, Ordering::Release);
            } else {
                // The bug: A's unconditional restore re-opened the table
                // while B is still inside its test.
                panic!("{}", EXPECTED);
            }
        });

        owner_a.join().unwrap();
        owner_b.join().unwrap();
        owner_c.join().unwrap();
    });
}
