//! A2b protocol model: transfer admission, coherent/disjoint grants and return.
//! No real heap, pointer provenance, VM, allocator bytes or Miri behavior.
#![forbid(unsafe_code)]
mod support;

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex, mpsc};
use loom::thread;
use support::negative as negative_control;

const TRANSFER_WITH_RUN: &str = "CATALOG_HEAP_BORROW_WITH_LIVE_RUN";
const TAIL_OVERLAP: &str = "CATALOG_UNCONDITIONAL_REWIND_OVERLAPS_LIVE_REGION";

#[derive(Default)]
struct AdmissionState {
    transition: bool,
    live: usize,
}
struct Admission {
    state: Mutex<AdmissionState>,
    heap_borrow: AtomicBool,
}
impl Admission {
    fn new() -> Self {
        Self {
            state: Mutex::new(AdmissionState::default()),
            heap_borrow: AtomicBool::new(false),
        }
    }
    fn run(&self) -> bool {
        let mut gate = self.state.lock().unwrap();
        if gate.transition {
            return false;
        }
        assert!(!self.heap_borrow.load(Ordering::Acquire));
        gate.live += 1;
        true
    }
    fn finish_run(&self) {
        let mut gate = self.state.lock().unwrap();
        assert!(gate.live > 0);
        assert!(!self.heap_borrow.load(Ordering::Acquire));
        gate.live -= 1;
    }
    fn preflight(&self, claim_transition: bool) -> bool {
        let mut gate = self.state.lock().unwrap();
        if gate.live != 0 || gate.transition {
            return false;
        }
        if claim_transition {
            gate.transition = true;
        }
        true
    }
    fn transfer(&self) {
        self.heap_borrow.store(true, Ordering::Release);
        {
            let gate = self.state.lock().unwrap();
            assert!(gate.transition);
            assert_eq!(gate.live, 0);
        }
        self.heap_borrow.store(false, Ordering::Release);
        self.state.lock().unwrap().transition = false;
    }
}

#[test]
fn catalog_transition_and_owner_run_share_one_admission_gate() {
    support::check(2, || {
        let admission = Arc::new(Admission::new());
        let remote = Arc::clone(&admission);
        let owner = thread::spawn(move || {
            if remote.run() {
                assert!(!remote.heap_borrow.load(Ordering::Acquire));
                remote.finish_run();
            }
        });
        // Either a preexisting run refuses transfer, or the claimed transition
        // refuses the new run through the complete modeled heap-borrow extent.
        if admission.preflight(true) {
            admission.transfer();
        }
        owner.join().unwrap();
        // Nonvacuous same-thread reuse after the complete transition and run.
        assert!(admission.run());
        assert!(!admission.preflight(true));
        admission.finish_run();
        assert!(admission.preflight(true));
        admission.transfer();
        assert_eq!(admission.state.lock().unwrap().live, 0);
    });
}

#[test]
fn negative_zero_loan_preflight_without_transition_allows_new_run() {
    negative_control(2, TRANSFER_WITH_RUN, || {
        let admission = Arc::new(Admission::new());
        let (start, start_rx) = mpsc::channel();
        let (live_tx, live_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let remote = Arc::clone(&admission);
        let owner = thread::spawn(move || {
            start_rx.recv().unwrap();
            assert!(remote.run());
            live_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            remote.finish_run();
        });
        // Bug: the zero-loan census alone does not reserve this phase.
        assert!(admission.preflight(false));
        start.send(()).unwrap();
        live_rx.recv().unwrap();
        admission.heap_borrow.store(true, Ordering::Release);
        let overlap = admission.state.lock().unwrap().live != 0;
        // Complete the finite actor DAG before the exact invariant panic;
        // no deadlock, branch exhaustion or joined-thread panic is a witness.
        admission.heap_borrow.store(false, Ordering::Release);
        release.send(()).unwrap();
        owner.join().unwrap();
        assert!(!overlap, "{TRANSFER_WITH_RUN}");
    });
}

#[derive(Clone, Copy, Debug)]
struct Span {
    first: usize,
    count: usize,
}
impl Span {
    fn end(self) -> usize {
        self.first + self.count
    }
    fn overlaps(self, other: Self) -> bool {
        self.first < other.end() && other.first < self.end()
    }
}
#[derive(Default)]
struct Catalog {
    allocator: bool,
    base_map: bool,
    cards: bool,
    chunk_map: bool,
    published: bool,
    next: usize,
    free: Option<Span>,
}
impl Catalog {
    fn grant(&mut self, want: usize) -> Span {
        assert!((1..=2).contains(&want));
        if !self.published {
            self.allocator = true;
            self.base_map = true;
            self.cards = true;
            self.chunk_map = true;
            // This handout publication is last in the catalog transaction.
            self.published = true;
        }
        assert!(self.allocator && self.base_map && self.cards && self.chunk_map);
        assert!(self.published);
        if let Some(mut free) = self.free.take() {
            let count = want.min(free.count);
            let grant = Span {
                first: free.first,
                count,
            };
            free.first += count;
            free.count -= count;
            if free.count != 0 {
                self.free = Some(free);
            }
            grant
        } else {
            let grant = Span {
                first: self.next,
                count: want,
            };
            self.next = grant.end();
            assert!(self.next <= 4);
            grant
        }
    }
    fn return_unused(&mut self, grant: Span, used: usize, unconditional: bool) {
        assert!(used <= grant.count);
        let unused = Span {
            first: grant.first + used,
            count: grant.count - used,
        };
        if unused.count == 0 {
            return;
        }
        if unconditional || self.next == unused.end() {
            self.next = unused.first;
        } else {
            assert!(self.free.is_none());
            self.free = Some(unused);
        }
    }
}

struct Cell {
    car: AtomicUsize,
    cdr: AtomicUsize,
    root: AtomicUsize,
}
impl Cell {
    fn new() -> Self {
        Self {
            car: AtomicUsize::new(0),
            cdr: AtomicUsize::new(0),
            root: AtomicUsize::new(0),
        }
    }
    fn initialize_and_root(&self, owner: usize) {
        // A second owner claiming the same cell cannot hide behind atomics.
        assert!(
            self.car
                .compare_exchange(0, owner, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        );
        self.cdr.store(owner + 10, Ordering::Relaxed);
        self.root.store(owner * 100 + 10, Ordering::Release);
    }
    fn check_root(&self, owner: usize) {
        assert_eq!(self.root.load(Ordering::Acquire), owner * 100 + 10);
        assert_eq!(self.car.load(Ordering::Relaxed), owner);
        assert_eq!(self.cdr.load(Ordering::Relaxed), owner + 10);
    }
}

#[test]
fn two_owner_catalog_grants_publish_all_metadata_and_root_before_run_return() {
    support::check(3, || {
        let admission = Arc::new(Admission::new());
        let catalog = Arc::new(Mutex::new(Catalog::default()));
        let cells = Arc::new([Cell::new(), Cell::new()]);
        let (ready, ready_rx) = mpsc::channel();
        let mut owners = Vec::new();
        let mut starts = Vec::new();
        for owner in 1..=2 {
            let (start, start_rx) = mpsc::channel();
            starts.push(start);
            let admission = Arc::clone(&admission);
            let catalog = Arc::clone(&catalog);
            let cells = Arc::clone(&cells);
            let ready = ready.clone();
            owners.push(thread::spawn(move || {
                assert!(admission.run());
                ready.send(()).unwrap();
                start_rx.recv().unwrap();
                let grant = catalog.lock().unwrap().grant(1);
                // Cell filling and root registration occur outside the gate.
                cells[grant.first].initialize_and_root(owner);
                cells[grant.first].check_root(owner);
                admission.finish_run();
                grant
            }));
        }
        ready_rx.recv().unwrap();
        ready_rx.recv().unwrap();
        assert_eq!(admission.state.lock().unwrap().live, 2);
        assert!(!admission.preflight(true));
        for start in starts {
            start.send(()).unwrap();
        }
        let left = owners.remove(0).join().unwrap();
        let right = owners.remove(0).join().unwrap();
        assert!(!left.overlaps(right));
        cells[left.first].check_root(1);
        cells[right.first].check_root(2);
        assert!(admission.preflight(true));
        admission.transfer();
    });
}

fn tail_schedule(unconditional: bool) -> bool {
    let catalog = Arc::new(Mutex::new(Catalog::default()));
    let cells = Arc::new([Cell::new(), Cell::new(), Cell::new(), Cell::new()]);
    let a = catalog.lock().unwrap().grant(1);
    let (held, held_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let remote_catalog = Arc::clone(&catalog);
    let remote_cells = Arc::clone(&cells);
    let owner = thread::spawn(move || {
        let b = remote_catalog.lock().unwrap().grant(1);
        remote_cells[b.first].initialize_and_root(2);
        held.send(b).unwrap();
        release_rx.recv().unwrap();
        remote_cells[b.first].check_root(2);
        b
    });
    let b = held_rx.recv().unwrap();
    assert!(!a.overlaps(b));
    // A's whole unused range precedes the live B grant. Rewinding to A now
    // is invalid even though A originally came from this same bump tail.
    catalog.lock().unwrap().return_unused(a, 0, unconditional);
    let third = catalog.lock().unwrap().grant(2);
    let overlap = third.overlaps(b);
    if !unconditional {
        assert_eq!(third.count, 1); // only A's returned free run is available
        cells[third.first].initialize_and_root(3);
        cells[third.first].check_root(3);
    }
    cells[b.first].check_root(2);
    release.send(()).unwrap();
    let completed_b = owner.join().unwrap();
    assert_eq!(completed_b.first, b.first);
    overlap
}

#[test]
fn earlier_tail_return_uses_free_run_without_overlapping_later_owner() {
    support::check(2, || {
        assert!(!tail_schedule(false));
    });
}

#[test]
fn negative_unconditional_tail_rewind_grants_live_owner_cells_again() {
    negative_control(2, TAIL_OVERLAP, || {
        let overlap = tail_schedule(true);
        assert!(!overlap, "{TAIL_OVERLAP}");
    });
}
