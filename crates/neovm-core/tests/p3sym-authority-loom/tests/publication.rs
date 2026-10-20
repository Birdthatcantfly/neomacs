mod model;
mod support;

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;
use model::{Authority, Checked, Latch};

#[derive(Clone, Copy, Default)]
struct Arms {
    value: usize,
    function: usize,
    name_object: usize,
}

struct Registry {
    ids: [Option<usize>; 2],
    next: usize,
}

struct World {
    authority: Authority,
    registry: Mutex<Registry>,
    arms: [Checked<Arms>; 2],
    names: [AtomicUsize; 2],
    members: Checked<usize>,
}

impl World {
    fn new() -> Self {
        Self {
            authority: Authority::new(),
            registry: Mutex::new(Registry {
                ids: [None, None],
                next: 1,
            }),
            arms: std::array::from_fn(|_| Checked::new(Arms::default())),
            names: std::array::from_fn(|_| AtomicUsize::new(0)),
            members: Checked::new(0),
        }
    }

    /// None means contention/retry, not a Lisp nil result. The registry guard
    /// and World word are distinct domains; all acquisition follows word→guard.
    fn intern(&self, name: usize, input_object: usize) -> Option<usize> {
        let _writer = self.authority.write()?;
        let mut registry = self.registry.lock().unwrap();
        if let Some(id) = registry.ids[name] {
            assert_eq!(self.names[name].load(Ordering::Acquire), id);
            return Some(id);
        }
        let id = registry.next;
        registry.next += 1;
        registry.ids[name] = Some(id);
        self.arms[name].set(Arms {
            value: 10 * id,
            function: 100 * id,
            name_object: input_object,
        });
        self.names[name].store(id, Ordering::Release);
        self.members.set(self.members.get() + 1);
        Some(id)
    }

    /// Outer None is Busy; Some(None) is a real miss with no side effects.
    fn soft(&self, name: usize) -> Option<Option<usize>> {
        let _reader = self.authority.read()?;
        let registry = self.registry.lock().unwrap();
        let id = registry.ids[name];
        if let Some(id) = id {
            assert_eq!(self.names[name].load(Ordering::Acquire), id);
            let arms = self.arms[name].get();
            assert_eq!(arms.value, 10 * id, "intern-soft saw partial arms");
            assert_eq!(arms.function, 100 * id, "intern-soft saw partial arms");
            assert_ne!(arms.name_object, 0, "intern-soft lost the name root");
        }
        Some(id)
    }
}

#[test]
fn same_name_interns_have_one_winner_and_soft_never_sees_half() {
    support::check(3, || {
        let world = Arc::new(World::new());
        let w = Arc::clone(&world);
        let one = thread::spawn(move || w.intern(0, 11));
        let w = Arc::clone(&world);
        let two = thread::spawn(move || w.intern(0, 22));
        let observed = world.soft(0);
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        // Resolve any Busy response after join, exactly as a wrapper retries
        // outside a lease. Both attempts may be Busy while soft holds a
        // reader; do not mistake that legal schedule for an intern failure.
        let one = one.or_else(|| world.intern(0, 11)).unwrap();
        let two = two.or_else(|| world.intern(0, 22)).unwrap();
        assert_eq!(one, two, "duplicate canonical symbol for one name");
        assert_eq!(world.members.get(), 1, "duplicate World membership");
        assert_eq!(world.registry.lock().unwrap().next, 2);
        if let Some(Some(id)) = observed {
            assert_eq!(id, one);
        }
        let original = world.arms[0].get().name_object;
        assert!(matches!(original, 11 | 22));
        assert_eq!(world.intern(0, 33), Some(one));
        assert_eq!(world.arms[0].get().name_object, original);
        assert_eq!(world.soft(0), Some(Some(one)));
        world.authority.assert_quiescent();
    });
}

#[test]
fn different_names_publish_distinct_ids_and_count() {
    support::check(3, || {
        let world = Arc::new(World::new());
        let a = Arc::clone(&world);
        let one = thread::spawn(move || a.intern(0, 11));
        let b = Arc::clone(&world);
        let two = thread::spawn(move || b.intern(1, 22));
        let one = one.join().unwrap().or_else(|| world.intern(0, 11));
        let two = two.join().unwrap().or_else(|| world.intern(1, 22));
        // The second owner may still hold WRITER during the first retry.
        let one = one.or_else(|| world.intern(0, 11)).unwrap();
        let two = two.or_else(|| world.intern(1, 22)).unwrap();
        assert_ne!(one, two);
        assert_eq!(world.members.get(), 2);
        assert_eq!(world.soft(0), Some(Some(one)));
        assert_eq!(world.soft(1), Some(Some(two)));
    });
}

#[test]
fn soft_miss_does_not_allocate_or_publish() {
    support::exhaustive(3, || {
        let world = Arc::new(World::new());
        let w = Arc::clone(&world);
        let reader = thread::spawn(move || {
            let result = w.soft(0);
            assert!(result.is_none() || result == Some(None));
        });
        // CAS competition may refuse a concurrent reader; retry after join.
        let first = world.soft(1);
        reader.join().unwrap();
        assert!(first.is_none() || first == Some(None));
        assert_eq!(world.soft(1), Some(None));
        assert_eq!(world.members.get(), 0);
        assert_eq!(world.registry.lock().unwrap().next, 1);
    });
}

/// Independent presence-publication submodel: unlike intern-soft (which also
/// holds authority), a presence-based snapshot reader relies on name Release.
/// Atomic payloads here permit the broken order to expose partial data safely;
/// checked ordinary payloads above cover intern's mutual exclusion separately.
fn presence_order(broken: bool) {
    let name = Arc::new(AtomicUsize::new(0));
    let arms = Arc::new(AtomicUsize::new(0));
    let (n, a) = (Arc::clone(&name), Arc::clone(&arms));
    let publisher = thread::spawn(move || {
        if broken {
            n.store(1, Ordering::Release);
            a.store(42, Ordering::Relaxed);
        } else {
            a.store(42, Ordering::Relaxed);
            n.store(1, Ordering::Release);
        }
    });
    if name.load(Ordering::Acquire) == 1 {
        assert!(
            arms.load(Ordering::Relaxed) == 42,
            "name published before symbol arms"
        );
    }
    publisher.join().unwrap();
}

#[test]
fn arms_before_name_release_publishes_complete_slot() {
    support::exhaustive(2, || presence_order(false));
}

#[test]
fn publication_before_arms_has_witness() {
    support::negative(2, "name published before symbol arms", || {
        presence_order(true)
    });
}

struct Spine {
    authority: Authority,
    slots: Checked<[usize; 2]>,
    generation: AtomicUsize,
    retired: AtomicUsize,
}

impl Spine {
    fn new() -> Self {
        Self {
            authority: Authority::new(),
            slots: Checked::new([11, 0]),
            generation: AtomicUsize::new(1),
            retired: AtomicUsize::new(0),
        }
    }

    fn grow(&self) {
        self.slots.set([11, 22]);
        let previous = self.generation.fetch_add(1, Ordering::Relaxed);
        self.retired.store(previous, Ordering::Relaxed);
    }
}

#[test]
fn spine_growth_is_exclusive_with_readers() {
    support::check(3, || {
        let spine = Arc::new(Spine::new());
        let s = Arc::clone(&spine);
        let reader = thread::spawn(move || {
            if let Some(_lease) = s.authority.read() {
                let generation = s.generation.load(Ordering::Relaxed);
                let slots = s.slots.get();
                assert!(matches!(slots, [11, 0] | [11, 22]));
                assert!(generation > s.retired.load(Ordering::Relaxed));
            }
        });
        let s = Arc::clone(&spine);
        let writer = thread::spawn(move || {
            if let Some(_lease) = s.authority.write() {
                s.grow();
            }
        });
        reader.join().unwrap();
        writer.join().unwrap();
        spine.authority.assert_quiescent();
    });
}

#[test]
fn unguarded_spine_growth_retires_live_reader_generation() {
    support::negative(2, "spine retired while reader retained generation", || {
        let spine = Arc::new(Spine::new());
        let entered = Arc::new(Latch::default());
        let grown = Arc::new(Latch::default());
        let (s, e, g) = (Arc::clone(&spine), Arc::clone(&entered), Arc::clone(&grown));
        let reader = thread::spawn(move || {
            let _lease = s.authority.read().unwrap();
            let generation = s.generation.load(Ordering::Relaxed);
            e.set();
            g.wait();
            // A freed-spine ledger models UAF without ever dereferencing an
            // actually freed Rust pointer, even when latches order the race.
            assert!(
                generation > s.retired.load(Ordering::Relaxed),
                "spine retired while reader retained generation"
            );
        });
        entered.wait();
        spine.grow(); // BUG: bypass WRITER and its reader drain.
        grown.set();
        reader.join().unwrap();
    });
}
