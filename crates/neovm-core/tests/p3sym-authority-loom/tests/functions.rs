//! C4 preparation: scalar function-state, stamp, activation and keeper models.
//! These do not admit concurrent production VM/JIT execution. Busy is a legal
//! finite-attempt result; joined actors have explicit serial completion suffixes.
mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use loom::thread;
use model::{Authority, Checked};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Definition {
    Empty,
    Unbound,
    Target(usize),
}

impl Definition {
    fn root(self) -> usize {
        match self {
            Self::Target(target) => 1 << target,
            Self::Empty | Self::Unbound => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cache {
    definition: Definition,
    clock: u64,
    refills: usize,
}

struct Functions {
    authority: Authority,
    cells: Checked<[Definition; 2]>,
    clock: AtomicU64,
    stamps: [AtomicU64; 2],
    dirty: Checked<usize>,
    quarantine: Checked<usize>,
    // One owner's activation home; a value copy, never a cell reference.
    activation: AtomicUsize,
}

impl Functions {
    fn new(cells: [Definition; 2]) -> Self {
        Self {
            authority: Authority::new(),
            cells: Checked::new(cells),
            clock: AtomicU64::new(1),
            stamps: std::array::from_fn(|_| AtomicU64::new(0)),
            dirty: Checked::new(0),
            quarantine: Checked::new(0),
            activation: AtomicUsize::new(0),
        }
    }

    fn commit(&self, symbol: usize, definition: Definition) -> Option<u64> {
        let _writer = self.authority.write()?;
        let mut cells = self.cells.get();
        let current = self.clock.load(Ordering::Acquire);
        if cells[symbol] == definition {
            return Some(current);
        }
        // Preflight wrap and journal space before the first mutation. The
        // bitset is a fixed, pre-reserved, deduplicating model-only journal.
        let next = current.checked_add(1).filter(|next| *next != u64::MAX)?;
        self.quarantine
            .set(self.quarantine.get() | cells[symbol].root());
        self.dirty.set(self.dirty.get() | (1 << symbol));
        cells[symbol] = definition;
        self.cells.set(cells);
        self.stamps[symbol].store(next, Ordering::Release);
        self.clock.store(next, Ordering::Release);
        Some(next)
    }

    fn observe(&self, symbol: usize) -> Option<Definition> {
        let _reader = self.authority.read()?;
        Some(self.cells.get()[symbol])
    }

    fn cached_call(&self, symbol: usize, old: Cache) -> Option<Cache> {
        let _reader = self.authority.read()?;
        // Clock FIRST. A stamp hit can advance an unrelated symbol's cached
        // clock without rereading its cell. Never reread the clock to label it.
        let clock = self.clock.load(Ordering::Acquire);
        let stamp = self.stamps[symbol].load(Ordering::Acquire);
        let current = if old.clock != u64::MAX && stamp <= old.clock {
            Cache { clock, ..old }
        } else {
            Cache {
                definition: self.cells.get()[symbol],
                clock,
                refills: old.refills + 1,
            }
        };
        assert!(
            current.definition == self.cells.get()[symbol],
            "restricted call accepted a retagged old definition"
        );
        self.activation
            .store(current.definition.root(), Ordering::Release);
        Some(current)
    }

    fn stopped_marks(&self) -> usize {
        self.authority.assert_quiescent();
        self.cells
            .get()
            .into_iter()
            .fold(0, |bits, d| bits | d.root())
            | self.activation.load(Ordering::Acquire)
            | self.quarantine.get()
    }

    fn reconcile(&self, legacy: &mut usize) {
        self.authority.assert_quiescent();
        // The legacy metadata is owner-confined and can still refer to the
        // quarantined definition until keeper eviction. Evict BEFORE clearing.
        *legacy = 0;
        self.dirty.set(0);
        self.quarantine.set(0);
    }
}

fn initial_cache(target: usize) -> Cache {
    Cache {
        definition: Definition::Target(target),
        clock: 1,
        refills: 0,
    }
}

#[test]
fn empty_to_explicit_unbound_is_a_change_then_a_noop() {
    support::exhaustive(1, || {
        let f = Functions::new([Definition::Empty; 2]);
        assert_eq!(f.commit(0, Definition::Unbound), Some(2));
        assert_eq!(f.observe(0), Some(Definition::Unbound));
        assert_eq!(f.stamps[0].load(Ordering::Acquire), 2);
        assert_eq!(f.commit(0, Definition::Unbound), Some(2));
        assert_eq!(f.commit(1, Definition::Empty), Some(2));
        assert_eq!(f.stamps[1].load(Ordering::Acquire), 0);
        assert_eq!(f.dirty.get(), 1);
        assert_eq!(f.commit(1, Definition::Target(4)), Some(3));
        assert_eq!(f.commit(1, Definition::Target(4)), Some(3));
        assert_eq!(f.stamps[1].load(Ordering::Acquire), 3);
        assert_eq!(f.quarantine.get(), 0);
        // MAX is the existing EMPTY/DISARMED cache sentinel, not a live era
        // clock. Exhaustion refuses before dirty/quarantine/cell mutation.
        f.clock.store(u64::MAX - 1, Ordering::Release);
        let cells = f.cells.get();
        let dirty = f.dirty.get();
        assert_eq!(f.commit(0, Definition::Target(5)), None);
        assert_eq!(f.cells.get(), cells);
        assert_eq!(f.dirty.get(), dirty);
        assert_eq!(f.quarantine.get(), 0);
    });
}

#[test]
fn different_symbol_fset_and_unbind_share_one_change_clock() {
    support::check(2, || {
        let f = Arc::new(Functions::new([
            Definition::Target(1),
            Definition::Target(2),
        ]));
        let other = Arc::clone(&f);
        let writer = thread::spawn(move || other.commit(0, Definition::Target(3)));
        let unbind = f.commit(1, Definition::Unbound);
        let redefine = writer.join().unwrap();
        // Refusal is legal. Completion after join needs no fairness assumption.
        if redefine.is_none() {
            assert!(f.commit(0, Definition::Target(3)).is_some());
        }
        if unbind.is_none() {
            assert!(f.commit(1, Definition::Unbound).is_some());
        }
        f.authority.assert_quiescent();
        assert_eq!(f.clock.load(Ordering::Acquire), 3);
        assert_eq!(f.observe(0), Some(Definition::Target(3)));
        assert_eq!(f.observe(1), Some(Definition::Unbound));
        let stamps = f.stamps.each_ref().map(|s| s.load(Ordering::Acquire));
        assert!(matches!(stamps, [2, 3] | [3, 2]));
        assert_eq!(f.dirty.get(), 3);
    });
}

#[test]
fn unrelated_change_resyncs_clock_without_refilling_target() {
    support::check(2, || {
        let f = Arc::new(Functions::new([
            Definition::Target(1),
            Definition::Target(2),
        ]));
        let other = Arc::clone(&f);
        let writer = thread::spawn(move || other.commit(1, Definition::Target(3)));
        let observed = f.cached_call(0, initial_cache(1));
        let committed = writer.join().unwrap();
        if committed.is_none() {
            assert_eq!(f.commit(1, Definition::Target(3)), Some(2));
        }
        if let Some(observed) = observed {
            assert_eq!(observed.definition, Definition::Target(1));
            assert_eq!(observed.refills, 0);
            assert!(matches!(observed.clock, 1 | 2));
        }
        assert_eq!(
            f.cached_call(0, initial_cache(1)),
            Some(Cache {
                clock: 2,
                ..initial_cache(1)
            })
        );
    });
}

#[test]
fn target_change_refills_while_unrelated_cache_only_resyncs() {
    support::exhaustive(1, || {
        let f = Functions::new([Definition::Target(1), Definition::Target(2)]);
        assert_eq!(f.commit(1, Definition::Target(3)), Some(2));
        let a = f.cached_call(0, initial_cache(1));
        assert_eq!(
            a,
            Some(Cache {
                clock: 2,
                ..initial_cache(1)
            })
        );
        assert_eq!(f.commit(0, Definition::Target(4)), Some(3));
        if let Some(a) = a {
            assert_eq!(
                f.cached_call(0, a),
                Some(Cache {
                    definition: Definition::Target(4),
                    clock: 3,
                    refills: 1
                })
            );
        }
        assert_eq!(f.stamps[1].load(Ordering::Acquire), 2);
    });
}

#[test]
fn reader_root_home_is_installed_before_writer_can_commit() {
    support::check(2, || {
        let f = Arc::new(Functions::new([Definition::Target(1), Definition::Empty]));
        // A live lease makes the other owner's finite attempt Busy. The root
        // home is populated before dropping that lease; no unchecked payload
        // access occurs outside either the lease or the joined stop.
        if let Some(reader) = f.authority.read() {
            let selected = f.cells.get()[0];
            let other = Arc::clone(&f);
            let writer = thread::spawn(move || other.commit(0, Definition::Target(2)));
            assert_eq!(writer.join().unwrap(), None);
            f.activation.store(selected.root(), Ordering::Release);
            drop(reader);
            assert_eq!(f.commit(0, Definition::Target(2)), Some(2));
            let mut legacy = 1 << 1;
            f.reconcile(&mut legacy);
            assert_ne!(f.stopped_marks() & (1 << 1), 0);
            f.activation.store(0, Ordering::Release);
            assert_eq!(f.stopped_marks() & (1 << 1), 0);
        } else {
            panic!("uncontended initial reader refused");
        }
    });
}

#[test]
fn unbind_preserves_active_old_call_but_next_call_is_unbound() {
    support::exhaustive(1, || {
        let f = Functions::new([Definition::Target(1), Definition::Empty]);
        let old = f.cached_call(0, initial_cache(1));
        assert_eq!(old, Some(initial_cache(1)));
        assert_eq!(f.commit(0, Definition::Unbound), Some(2));
        let mut legacy = 1 << 1;
        f.reconcile(&mut legacy);
        assert_ne!(f.stopped_marks() & (1 << 1), 0);
        // Complete the old activation before selecting its successor.
        f.activation.store(0, Ordering::Release);
        if let Some(old) = old {
            assert_eq!(
                f.cached_call(0, old),
                Some(Cache {
                    definition: Definition::Unbound,
                    clock: 2,
                    refills: 1
                })
            );
        }
        assert_eq!(f.stopped_marks() & (1 << 1), 0);
        assert_eq!(f.commit(0, Definition::Target(2)), Some(3));
    });
}

#[test]
fn deduplicated_quarantine_lives_until_stopped_reconciliation() {
    support::exhaustive(1, || {
        let f = Functions::new([Definition::Target(1), Definition::Target(1)]);
        let mut legacy = 1 << 1;
        assert_eq!(f.commit(0, Definition::Target(2)), Some(2));
        assert_eq!(f.commit(1, Definition::Target(2)), Some(3));
        assert_eq!(f.quarantine.get(), 1 << 1);
        assert_eq!(f.dirty.get(), 3);
        assert_eq!(f.stopped_marks() & legacy, legacy);
        f.reconcile(&mut legacy);
        assert_eq!(legacy, 0);
        assert_eq!(f.dirty.get(), 0);
        assert_eq!(f.quarantine.get(), 0);
        assert_eq!(f.stopped_marks(), 1 << 2);
        // A later era also releases its old definition; quarantine is bounded
        // to pending reconciliation, not every function ever assigned.
        assert_eq!(f.commit(0, Definition::Target(3)), Some(4));
        assert_eq!(f.commit(1, Definition::Target(3)), Some(5));
        legacy = 1 << 2;
        f.reconcile(&mut legacy);
        assert_eq!(f.stopped_marks(), 1 << 3);
    });
}

#[test]
fn retagging_old_cache_with_a_later_clock_is_rejected() {
    support::negative(
        2,
        "restricted call accepted a retagged old definition",
        || {
            let f = Arc::new(Functions::new([Definition::Target(1), Definition::Empty]));
            let old = f.cached_call(0, initial_cache(1));
            let other = Arc::clone(&f);
            assert_eq!(
                thread::spawn(move || other.commit(0, Definition::Target(2)))
                    .join()
                    .unwrap(),
                Some(2)
            );
            if let Some(mut old) = old {
                // Broken resync labels a prior protected target with a clock
                // observed after releasing authority and after a redefinition.
                old.clock = f.clock.load(Ordering::Acquire);
                let _ = f.cached_call(0, old);
            }
        },
    );
}

#[test]
fn collapsing_empty_and_explicit_unbound_omits_a_real_change() {
    support::negative(1, "explicit unbound transition omitted its stamp", || {
        let f = Functions::new([Definition::Empty; 2]);
        // Deliberately broken no-op classifier: both read as Lisp nil, but
        // their representation/state transition is not interchangeable.
        if f.cells.get()[0].root() == Definition::Unbound.root() {
            assert!(
                f.stamps[0].load(Ordering::Acquire) == 2,
                "explicit unbound transition omitted its stamp"
            );
        }
    });
}

#[test]
fn clearing_quarantine_before_legacy_eviction_has_a_witness() {
    support::negative(
        1,
        "legacy cache target lost before stopped eviction",
        || {
            let f = Functions::new([Definition::Target(1), Definition::Empty]);
            let legacy = 1 << 1;
            assert_eq!(f.commit(0, Definition::Target(2)), Some(2));
            // Broken keeper order releases the only old-definition root while
            // stopped legacy metadata still refers to it.
            f.quarantine.set(0);
            assert!(
                f.stopped_marks() & legacy == legacy,
                "legacy cache target lost before stopped eviction"
            );
        },
    );
}
