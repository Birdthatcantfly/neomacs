mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::thread;
use model::{Authority, Checked};

struct Functions {
    authority: Authority,
    cells: Checked<[usize; 2]>,
    stamps: [AtomicU64; 2],
    epoch: AtomicU64,
}

#[derive(Clone, Copy)]
struct Cache {
    epoch: u64,
    target: usize,
}

/// An owner activation root stands for a rooted copy of a selected target;
/// it is a value copy, not a borrowed pointer back into the symbol's cell.
struct Activation {
    rooted_target: usize,
    selected_epoch: u64,
}

impl Functions {
    fn new() -> Self {
        Self {
            authority: Authority::new(),
            cells: Checked::new([10, 20]),
            stamps: std::array::from_fn(|_| AtomicU64::new(1)),
            epoch: AtomicU64::new(1),
        }
    }

    fn fset(&self, symbol: usize, target: usize, bump: bool) -> Option<u64> {
        let _writer = self.authority.write()?;
        let mut cells = self.cells.get();
        if cells[symbol] == target {
            return Some(self.epoch.load(Ordering::Acquire));
        }
        cells[symbol] = target;
        self.cells.set(cells);
        if bump {
            // The coarse World word serializes load+next+store even for
            // different symbols. This is the existing clock, not a seqlock.
            let next = self.epoch.load(Ordering::Acquire) + 1;
            self.stamps[symbol].store(next, Ordering::Release);
            self.epoch.store(next, Ordering::Release);
        }
        Some(self.epoch.load(Ordering::Acquire))
    }

    fn call(&self, cache: &mut Cache) -> Option<Activation> {
        let _reader = self.authority.read()?;
        let epoch = self.epoch.load(Ordering::Acquire);
        if cache.epoch != epoch {
            cache.target = self.cells.get()[0];
            cache.epoch = epoch;
        }
        assert!(
            cache.target == self.cells.get()[0],
            "call accepted stale target at unchanged epoch"
        );
        // In the engine this copy must go into the caller's real root home
        // before read authority is released. Here the owned activation models
        // that placement; GC/lifetime bookkeeping is exercised below.
        Some(Activation {
            rooted_target: cache.target,
            selected_epoch: epoch,
        })
    }
}

#[test]
fn concurrent_call_revalidates_complete_target_and_epoch() {
    support::check(3, || {
        let functions = Arc::new(Functions::new());
        let f = Arc::clone(&functions);
        let writer = thread::spawn(move || f.fset(0, 11, true));
        let f = Arc::clone(&functions);
        let caller = thread::spawn(move || {
            let mut cache = Cache {
                epoch: 1,
                target: 10,
            };
            f.call(&mut cache)
        });
        let committed = writer.join().unwrap();
        let activation = caller.join().unwrap();
        if let Some(activation) = activation {
            assert!(matches!(
                (activation.rooted_target, activation.selected_epoch),
                (10, 1) | (11, 2)
            ));
        }
        if committed.is_none() {
            functions.fset(0, 11, true).unwrap();
        }
        let mut stale = Cache {
            epoch: 1,
            target: 10,
        };
        assert_eq!(functions.call(&mut stale).unwrap().rooted_target, 11);
        assert_eq!(stale.epoch, 2);
        assert_eq!(functions.stamps[0].load(Ordering::Acquire), 2);
    });
}

#[test]
fn different_symbol_writers_cannot_lose_epoch_increment() {
    support::check(3, || {
        let functions = Arc::new(Functions::new());
        let f = Arc::clone(&functions);
        let one = thread::spawn(move || f.fset(0, 11, true));
        let f = Arc::clone(&functions);
        let two = thread::spawn(move || f.fset(1, 21, true));
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        let one = one.or_else(|| functions.fset(0, 11, true)).unwrap();
        let two = two.or_else(|| functions.fset(1, 21, true)).unwrap();
        assert_ne!(one, two, "different fsets reused one epoch");
        assert_eq!(functions.epoch.load(Ordering::Acquire), 3);
        assert_eq!(functions.cells.get(), [11, 21]);
        let a = functions.stamps[0].load(Ordering::Acquire);
        let b = functions.stamps[1].load(Ordering::Acquire);
        assert!(matches!((a, b), (2, 3) | (3, 2)));
    });
}

#[test]
fn same_definition_does_not_bump_epoch() {
    support::exhaustive(2, || {
        let functions = Arc::new(Functions::new());
        let f = Arc::clone(&functions);
        let writer = thread::spawn(move || f.fset(0, 10, true));
        let _ = writer.join().unwrap();
        assert_eq!(functions.epoch.load(Ordering::Acquire), 1);
        assert_eq!(functions.stamps[0].load(Ordering::Acquire), 1);
    });
}

#[test]
fn missing_epoch_bump_accepts_stale_call() {
    support::negative(2, "call accepted stale target at unchanged epoch", || {
        let functions = Arc::new(Functions::new());
        let f = Arc::clone(&functions);
        thread::spawn(move || f.fset(0, 11, false)).join().unwrap();
        let mut cache = Cache {
            epoch: 1,
            target: 10,
        };
        functions.call(&mut cache).unwrap();
    });
}

fn old_activation_gc(keep_root: bool) {
    use loom::sync::atomic::AtomicUsize;
    use model::Latch;

    let functions = Arc::new(Functions::new());
    let selected = Arc::new(Latch::default());
    let collected = Arc::new(Latch::default());
    // A bit per target models same-heap root/mark membership. No physical
    // GC object is allocated or freed; the negative detects a dead target
    // before attempting any dereference.
    let activation_root = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let (f, s, c, roots, marks) = (
        Arc::clone(&functions),
        Arc::clone(&selected),
        Arc::clone(&collected),
        Arc::clone(&activation_root),
        Arc::clone(&live),
    );
    let caller = thread::spawn(move || {
        let mut cache = Cache {
            epoch: 1,
            target: 10,
        };
        let activation = f.call(&mut cache).unwrap();
        if keep_root {
            roots.store(1 << activation.rooted_target, Ordering::Release);
        }
        // Modeled ACK: read lease already ended, activation root published.
        s.set();
        c.wait();
        assert!(
            marks.load(Ordering::Acquire) & (1 << activation.rooted_target) != 0,
            "active old function reclaimed after fset"
        );
        roots.store(0, Ordering::Release);
    });
    selected.wait();
    functions.fset(0, 11, true).unwrap();
    functions.authority.assert_quiescent();
    let symbol_root = 1 << functions.cells.get()[0];
    live.store(
        symbol_root | activation_root.load(Ordering::Acquire),
        Ordering::Release,
    );
    collected.set();
    caller.join().unwrap();
    // Next stopped cycle: the old activation has returned, so it can die.
    let next_marks = symbol_root | activation_root.load(Ordering::Acquire);
    assert_eq!(next_marks & (1 << 10), 0, "old activation retained forever");
    assert_ne!(next_marks & (1 << 11), 0);
}

#[test]
fn active_old_function_root_survives_then_reclaims() {
    support::check(2, || old_activation_gc(true));
}

#[test]
fn missing_activation_root_has_witness() {
    support::negative(2, "active old function reclaimed after fset", || {
        old_activation_gc(false)
    });
}
