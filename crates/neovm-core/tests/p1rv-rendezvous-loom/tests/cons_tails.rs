//! Owner-exclusive cons tail model: batch grants of whole tails, lock-free
//! local carving, and exact publication at ACK/native stop. No real heap,
//! pointer provenance, VM, allocator bytes or Miri behavior.
#![forbid(unsafe_code)]
mod support;

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex, mpsc};
use loom::thread;
use support::negative as negative_control;

const CARVE_PAST_LIMIT: &str = "TAIL_CARVE_BEYOND_LENT_RANGE_OVERLAPS_NEXT_BATCH";
const PUBLISH_BELOW_CURSOR: &str = "TAIL_PUBLISH_BELOW_HANDED_CURSOR_REGRANTS_LIVE_CELLS";
const SKIP_PUBLISH: &str = "TAIL_ACK_WITHOUT_PUBLISH_RETURNS_OVERLIVED_STORAGE";

const STORAGE: usize = 8;

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

/// The cold catalog gate. Every tail lent here is exclusively owned by one
/// mutator until its publication returns the unhanded suffix.
#[derive(Default)]
struct CatalogState {
    next: usize,
    free: Option<Span>,
    active_tails: usize,
    live: usize,
}
struct Catalog {
    state: Mutex<CatalogState>,
}
impl Catalog {
    fn new() -> Self {
        Self {
            state: Mutex::new(CatalogState::default()),
        }
    }
    /// One gate acquisition lends up to `want` cells as whole disjoint tails.
    fn grant_tail_batch(&self, want: usize) -> Vec<Span> {
        let mut gate = self.state.lock().unwrap();
        let mut tails = Vec::new();
        let mut remaining = want;
        while remaining > 0 && tails.len() < 2 {
            if let Some(free) = gate.free.take() {
                let count = free.count.min(remaining);
                tails.push(Span {
                    first: free.first,
                    count,
                });
                let rest = Span {
                    first: free.first + count,
                    count: free.count - count,
                };
                if rest.count != 0 {
                    gate.free = Some(rest);
                }
                remaining -= count;
            } else {
                assert!(gate.next + remaining <= STORAGE, "modeled storage bound");
                tails.push(Span {
                    first: gate.next,
                    count: remaining,
                });
                gate.next += remaining;
                remaining = 0;
            }
            gate.active_tails += 1;
            gate.live += tails.last().unwrap().count;
        }
        tails
    }
    /// The ACK-side publication: return the unhanded suffix exactly, rewind the
    /// bump top only when this tail owns it, and refund only that suffix.
    fn publish_tail(&self, tail: Span, from_tail: bool) {
        let mut gate = self.state.lock().unwrap();
        assert!(gate.active_tails > 0);
        if tail.count != 0 {
            if from_tail && gate.next == tail.end() {
                gate.next = tail.first;
            } else {
                assert!(gate.free.is_none(), "one modeled free run");
                gate.free = Some(tail);
            }
            assert!(gate.live >= tail.count);
            gate.live -= tail.count;
        }
        gate.active_tails -= 1;
    }
    /// Stopped keeper view: every tail published, live equals handed cells.
    fn stopped(&self) -> usize {
        let gate = self.state.lock().unwrap();
        assert_eq!(gate.active_tails, 0, "every tail published before restore");
        gate.live
    }
    fn active(&self) -> usize {
        self.state.lock().unwrap().active_tails
    }
}

/// Owner-local carving state; no shared gate is taken between batch grants.
struct OwnedTail {
    span: Span,
    from_tail: bool,
}
impl OwnedTail {
    fn carve(&mut self, want: usize) -> Span {
        let count = want.min(self.span.count);
        assert!(count != 0, "caller refills on exhaustion");
        let region = Span {
            first: self.span.first,
            count,
        };
        self.span.first += count;
        self.span.count -= count;
        region
    }
    /// Local close: hand the region suffix back to this same tail.
    fn rewind_region(&mut self, region: Span, used: usize) -> bool {
        if self.span.first != region.end() {
            return false;
        }
        self.span.first = region.first + used;
        self.span.count += region.count - used;
        true
    }
}

struct Cell {
    root: AtomicUsize,
}
impl Cell {
    fn new() -> Self {
        Self {
            root: AtomicUsize::new(0),
        }
    }
    fn initialize_and_root(&self, owner: usize) {
        assert!(
            self.root
                .compare_exchange(0, owner, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok(),
            "two owners carved the same cell"
        );
    }
    fn check_root(&self, owner: usize) {
        assert_eq!(self.root.load(Ordering::Acquire), owner);
    }
}

#[test]
fn exclusive_tails_carve_disjoint_cells_and_publish_exactly_at_ack() {
    support::check(3, || {
        let catalog = Arc::new(Catalog::new());
        let cells = Arc::new([
            Cell::new(),
            Cell::new(),
            Cell::new(),
            Cell::new(),
            Cell::new(),
            Cell::new(),
            Cell::new(),
            Cell::new(),
        ]);
        let mut acks = Vec::new();
        let mut resumes = Vec::new();
        let mut owners = Vec::new();
        for owner in 1..=2usize {
            let (ack, ack_rx) = mpsc::channel();
            let (resume, resume_rx) = mpsc::channel();
            let catalog = Arc::clone(&catalog);
            let cells = Arc::clone(&cells);
            owners.push(thread::spawn(move || {
                let mut tails: Vec<OwnedTail> = catalog
                    .grant_tail_batch(2)
                    .into_iter()
                    .map(|span| OwnedTail {
                        span,
                        from_tail: true,
                    })
                    .collect();
                // Carve, initialize and root cells with no shared gate held.
                let region = tails.first_mut().unwrap().carve(1);
                cells[region.first].initialize_and_root(owner);
                assert!(tails.first_mut().unwrap().rewind_region(region, region.count));
                // ACK: the region closed locally, then every tail publishes.
                for tail in tails.drain(..) {
                    catalog.publish_tail(tail.span, tail.from_tail);
                }
                ack.send(region.first).unwrap();
                resume_rx.recv().unwrap();
                // After the collector's resume a new batch reuses returned
                // cells without touching the rooted live ones.
                let mut tails: Vec<OwnedTail> = catalog
                    .grant_tail_batch(1)
                    .into_iter()
                    .map(|span| OwnedTail {
                        span,
                        from_tail: true,
                    })
                    .collect();
                let region = tails.first_mut().unwrap().carve(1);
                cells[region.first].initialize_and_root(owner + 10);
                assert!(tails.first_mut().unwrap().rewind_region(region, region.count));
                for tail in tails.drain(..) {
                    catalog.publish_tail(tail.span, tail.from_tail);
                }
                ack.send(region.first).unwrap();
            }));
            acks.push(ack_rx);
            resumes.push(resume);
        }
        // Keeper: both owners ACKed; the catalog is one coherent picture.
        let first_rooted = acks[0].recv().unwrap();
        let second_rooted = acks[1].recv().unwrap();
        assert_ne!(first_rooted, second_rooted);
        assert_eq!(catalog.stopped(), 2, "only handed cells stay charged");
        for resume in &resumes {
            resume.send(()).unwrap();
        }
        let again_first = acks[0].recv().unwrap();
        let again_second = acks[1].recv().unwrap();
        assert!(again_first != first_rooted || again_second != second_rooted);
        assert_eq!(catalog.stopped(), 4);
        for owner in owners {
            owner.join().unwrap();
        }
    });
}

fn tail_schedule(bug: &str) {
    let catalog = Arc::new(Catalog::new());
    let cells = Arc::new([Cell::new(), Cell::new(), Cell::new(), Cell::new()]);
    let mut a = OwnedTail {
        span: catalog.grant_tail_batch(2).remove(0),
        from_tail: true,
    };
    let remote = Arc::clone(&catalog);
    let remote_cells = Arc::clone(&cells);
    let (held, held_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let owner = thread::spawn(move || {
        let mut b = OwnedTail {
            span: remote.grant_tail_batch(2).remove(0),
            from_tail: true,
        };
        let region = b.carve(1);
        remote_cells[region.first].initialize_and_root(2);
        assert!(b.rewind_region(region, region.count));
        held.send((region, b.span)).unwrap();
        release_rx.recv().unwrap();
        remote_cells[region.first].check_root(2);
    });
    let (b_live, b_lent) = held_rx.recv().unwrap();
    let region = a.carve(1);
    cells[region.first].initialize_and_root(1);
    let a_live = region;
    let used = if bug == CARVE_PAST_LIMIT { 0 } else { region.count };
    assert!(a.rewind_region(region, used));
    match bug {
        SKIP_PUBLISH => {}
        PUBLISH_BELOW_CURSOR => {
            // Bug: publish rewinds below the handed cursor, offering the live
            // cell back to the catalog's free run.
            catalog.publish_tail(Span { first: 0, count: 2 }, true);
        }
        _ => catalog.publish_tail(a.span, a.from_tail),
    }
    if bug == SKIP_PUBLISH {
        assert!(catalog.active() == 0, "{bug}");
    }
    let third = catalog.grant_tail_batch(2);
    let overlap = third
        .iter()
        .any(|span| span.overlaps(a_live) || span.overlaps(b_lent));
    cells[b_live.first].check_root(2);
    release.send(()).unwrap();
    owner.join().unwrap();
    assert!(!overlap, "{bug}");
}

#[test]
fn published_tails_never_overlap_later_owner_grants() {
    support::check(2, || {
        tail_schedule("");
    });
}

#[test]
fn negative_carve_beyond_lent_tail_overlaps_next_batch() {
    negative_control(2, CARVE_PAST_LIMIT, || {
        tail_schedule(CARVE_PAST_LIMIT);
    });
}

#[test]
fn negative_publish_below_handed_cursor_regrants_live_cells() {
    negative_control(2, PUBLISH_BELOW_CURSOR, || {
        tail_schedule(PUBLISH_BELOW_CURSOR);
    });
}

#[test]
fn negative_ack_without_publish_keeps_storage_overlived() {
    negative_control(2, SKIP_PUBLISH, || {
        tail_schedule(SKIP_PUBLISH);
    });
}
