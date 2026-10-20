mod model;
mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::thread;
use model::{Authority, Cell, Latch, Writer};

struct World {
    authority: Authority,
    cell: Cell,
    stop: AtomicBool,
    ack: [AtomicBool; 2],
    poisoned: AtomicBool,
}

#[derive(Debug, PartialEq)]
enum Refusal {
    NotStopped,
    Busy,
    Poisoned,
}

impl World {
    fn new() -> Self {
        Self {
            authority: Authority::new(),
            cell: Cell::new(),
            stop: AtomicBool::new(false),
            ack: std::array::from_fn(|_| AtomicBool::new(false)),
            poisoned: AtomicBool::new(false),
        }
    }

    fn trace(&self) -> Result<(usize, usize), Refusal> {
        self.trace_with_word_gate(true)
    }

    fn trace_with_word_gate(&self, check_word: bool) -> Result<(usize, usize), Refusal> {
        if !self.stop.load(Ordering::Acquire)
            || self.ack.iter().any(|ack| !ack.load(Ordering::Acquire))
        {
            return Err(Refusal::NotStopped);
        }
        if check_word && self.authority.word.load(Ordering::Acquire) != 0 {
            return Err(Refusal::Busy);
        }
        if self.poisoned.load(Ordering::Acquire) {
            return Err(Refusal::Poisoned);
        }
        // Independent trace-entry invariant. Disabling only the admission
        // gate must reach this witness, before a checked payload dereference
        // can turn it into a less precise data-race report.
        assert!(
            self.authority.word.load(Ordering::Acquire) == 0,
            "keeper traced under nonzero symbol authority"
        );
        Ok(self.cell.copy())
    }

    fn resumable(&self) -> bool {
        !self.poisoned.load(Ordering::Acquire)
    }
}

#[test]
fn keeper_waits_for_ack_after_authority_release() {
    support::check(2, || {
        let world = Arc::new(World::new());
        let w = Arc::clone(&world);
        let owner = thread::spawn(move || {
            let writer = w.authority.write().unwrap();
            w.cell.set(2);
            drop(writer);
            // This is the rendezvous publication edge, not a fresh lock
            // around the cell. No owner re-enters until keeper resumes it.
            w.ack[0].store(true, Ordering::Release);
        });
        world.stop.store(true, Ordering::Release);
        world.ack[1].store(true, Ordering::Release);
        if let Ok((value, version)) = world.trace() {
            assert_eq!((value, version), (20, 2));
        }
        owner.join().unwrap();
        assert_eq!(world.trace(), Ok((20, 2)));
        world.authority.assert_quiescent();
    });
}

#[test]
fn zero_word_without_all_acknowledgements_is_not_stop_authority() {
    support::exhaustive(1, || {
        let world = World::new();
        world.stop.store(true, Ordering::Release);
        world.ack[0].store(true, Ordering::Release);
        assert_eq!(world.trace(), Err(Refusal::NotStopped));
    });
}

#[test]
fn contender_can_ack_stop_outside_failed_writer_attempt() {
    support::check(2, || {
        let world = Arc::new(World::new());
        let first_reader = world.authority.read().unwrap();
        let refused = Arc::new(Latch::default());
        let poll = Arc::new(Latch::default());
        let (w, r, p) = (Arc::clone(&world), Arc::clone(&refused), Arc::clone(&poll));
        let contender = thread::spawn(move || {
            assert!(w.authority.write().is_none());
            r.set();
            p.wait();
            assert!(w.stop.load(Ordering::Acquire));
            // The failed attempt held no lease. It does not spin for the
            // reader or require that owner's Baton before publishing ACK.
            w.ack[1].store(true, Ordering::Release);
        });
        refused.wait();
        world.stop.store(true, Ordering::Release);
        poll.set();
        contender.join().unwrap();
        assert_eq!(world.trace(), Err(Refusal::NotStopped));
        drop(first_reader);
        world.ack[0].store(true, Ordering::Release);
        assert_eq!(world.trace(), Ok((10, 1)));
    });
}

#[test]
fn keeper_rejects_ack_with_live_word() {
    support::exhaustive(1, || {
        let world = World::new();
        let _forgotten_writer = world.authority.write().unwrap();
        // Fault injection stands for a broken ACK/forgotten lease. The
        // keeper must refuse even though its ACK census looks complete.
        world.stop.store(true, Ordering::Release);
        for ack in &world.ack {
            ack.store(true, Ordering::Release);
        }
        assert_eq!(world.trace(), Err(Refusal::Busy));
    });
}

#[test]
fn keeper_ignoring_word_zero_has_witness() {
    support::negative(2, "keeper traced under nonzero symbol authority", || {
        let world = Arc::new(World::new());
        let writer = world.authority.write().unwrap();
        world.stop.store(true, Ordering::Release);
        for ack in &world.ack {
            ack.store(true, Ordering::Release);
        }
        let w = Arc::clone(&world);
        thread::spawn(move || {
            // Mutation control: bypass just the zero-word gate in the same
            // trace implementation used by the passing tests.
            let _ = w.trace_with_word_gate(false);
        })
        .join()
        .unwrap();
        drop(writer);
    });
}

/// Field drop releases WRITER only after Drop has poisoned an unfinished
/// transaction. Early return is the failure injection; we never catch a
/// Loom assertion inside an actor and mistake it for an engine exception.
struct Commit<'a> {
    world: &'a World,
    _writer: Writer<'a>,
    finished: bool,
    poison_on_failure: bool,
}

impl<'a> Commit<'a> {
    fn begin(world: &'a World, poison_on_failure: bool) -> Self {
        Self {
            world,
            _writer: world.authority.write().unwrap(),
            finished: false,
            poison_on_failure,
        }
    }

    fn finish(mut self) {
        self.finished = true;
    }
}

impl Drop for Commit<'_> {
    fn drop(&mut self) {
        if !self.finished && self.poison_on_failure {
            self.world.poisoned.store(true, Ordering::Release);
        }
    }
}

/// Fault injection completes only the first of two ordinary field writes.
fn partial_commit(world: &World, poison: bool, entered: Option<&Latch>) {
    let _unfinished = Commit::begin(world, poison);
    if let Some(entered) = entered {
        entered.set();
    }
    world.cell.value.set(20);
}

#[test]
fn partial_commit_poison_prevents_trace_and_resume() {
    support::check(2, || {
        let world = Arc::new(World::new());
        let entered = Arc::new(Latch::default());
        let w = Arc::clone(&world);
        let e = Arc::clone(&entered);
        let failed_writer = thread::spawn(move || {
            partial_commit(&w, true, Some(&e));
            w.ack[0].store(true, Ordering::Release);
        });
        // The next owner's probe races poison/release, but cannot steal the
        // fault-injected writer's scripted first acquisition.
        entered.wait();
        world.stop.store(true, Ordering::Release);
        world.ack[1].store(true, Ordering::Release);
        // A correctly admitted next owner checks poison under the acquired
        // word; it must not dereference the half-committed ordinary fields.
        if let Some(_lease) = world.authority.read()
            && !world.poisoned.load(Ordering::Acquire)
        {
            let (value, version) = world.cell.copy();
            assert_eq!(value, version * 10);
        }
        failed_writer.join().unwrap();
        assert_eq!(world.trace(), Err(Refusal::Poisoned));
        assert!(!world.resumable());
        world.authority.assert_quiescent();
    });
}

#[test]
fn partial_commit_without_poison_has_witness() {
    support::negative(2, "unpoisoned partial commit reached keeper", || {
        let world = Arc::new(World::new());
        let w = Arc::clone(&world);
        thread::spawn(move || {
            partial_commit(&w, false, None);
            w.ack[0].store(true, Ordering::Release);
        })
        .join()
        .unwrap();
        world.stop.store(true, Ordering::Release);
        world.ack[1].store(true, Ordering::Release);
        let (value, version) = world.trace().unwrap();
        assert!(
            value == version * 10,
            "unpoisoned partial commit reached keeper"
        );
    });
}

#[test]
fn completed_commit_remains_resumable() {
    support::exhaustive(2, || {
        let world = Arc::new(World::new());
        let w = Arc::clone(&world);
        thread::spawn(move || {
            let commit = Commit::begin(&w, true);
            w.cell.set(2);
            commit.finish();
            w.ack[0].store(true, Ordering::Release);
        })
        .join()
        .unwrap();
        world.stop.store(true, Ordering::Release);
        world.ack[1].store(true, Ordering::Release);
        assert_eq!(world.trace(), Ok((20, 2)));
        assert!(world.resumable());
    });
}
