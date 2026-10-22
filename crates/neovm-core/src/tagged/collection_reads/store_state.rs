//! World-owned store protocol. Only the Baton holder mutates these Cells.
//!
//! A collector may publish another owner's windows only while the registered
//! World is parked in the stopped handshake, before releasing that owner.
//! Neither a registry lock alone nor a shared heap reference permits this.
use super::*;
use crate::tagged::gc::{BarrierWindow, TaggedHeap};
use std::cell::OnceCell;
use std::mem::{MaybeUninit, align_of, offset_of, size_of};
use std::ptr::NonNull;
use std::rc::Rc;

/// Single home of the store protocol, retained by the World's cold registry
/// and its carried observation handle. All mutation uses interior mutability;
/// no `&mut StoreState` or unique Box reborrow can invalidate a Context root.
#[repr(C)]
pub(crate) struct StoreState {
    pub(super) revision: Cell<u64>,
    pub(super) policy: Cell<JournalPolicy>,
    pub(super) observe: Cell<bool>,
    pub(crate) cons_window: Cell<BarrierWindow>,
    pub(crate) window: Cell<BarrierWindow>,
    pub(crate) heap: Cell<*mut TaggedHeap>,
    pub(super) writes: [Cell<usize>; JOURNAL_SIZE],
}

impl StoreState {
    fn new() -> Self {
        initialize();
        Self {
            revision: Cell::new(0),
            policy: Cell::new(if history_required() {
                JournalPolicy::JournalOnly
            } else {
                JournalPolicy::BeforeFirstCapture
            }),
            observe: Cell::new(false),
            cons_window: Cell::new(BarrierWindow::NONE),
            window: Cell::new(BarrierWindow::NONE),
            heap: Cell::new(std::ptr::null_mut()),
            writes: [const { Cell::new(0) }; JOURNAL_SIZE],
        }
    }

    #[inline(always)]
    pub(crate) fn sequence(&self) -> u64 {
        self.revision.get()
    }

    #[inline(always)]
    pub(crate) fn advance(&self) -> u64 {
        let next = self.revision.get().wrapping_add(1);
        self.revision.set(next);
        next
    }

    #[inline(always)]
    pub(super) fn record(&self, owner: usize, revision: u64) {
        self.writes[revision as usize & (JOURNAL_SIZE - 1)].set(owner);
    }

    pub(crate) fn publish_windows(&self, cons: BarrierWindow, ordinary: BarrierWindow) {
        self.cons_window.set(cons);
        self.window.set(ordinary);
    }
}

struct WorldObservations {
    store: StoreState,
    dump: Cell<BarrierWindow>,
    // Keep the cold capture/envelope allocation lazy. Installing an empty
    // heap must not initialize collection-read metadata.
    cold: OnceCell<RefCell<State>>,
    bound: Cell<bool>,
}

/// Allocation lifetime only. Executing authority is the existing World Baton
/// or stopped-owner handshake; cloning this handle never grants execution.
#[derive(Clone)]
pub(crate) struct ObservationWorld(Rc<WorldObservations>);

static_assertions::assert_not_impl_any!(ObservationWorld: Send, Sync);

impl Default for ObservationWorld {
    fn default() -> Self {
        Self(Rc::new(WorldObservations {
            store: StoreState::new(),
            dump: Cell::new(BarrierWindow::NONE),
            cold: OnceCell::new(),
            bound: Cell::new(false),
        }))
    }
}

impl std::fmt::Debug for ObservationWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ObservationWorld")
            .field(&self.root())
            .finish()
    }
}

impl ObservationWorld {
    pub(super) fn current() -> Self {
        STATE.with(|slot| slot.owner.borrow().clone())
    }

    pub(super) fn is_current(&self) -> bool {
        current_store_root() == self.root()
    }

    pub(super) fn store(&self) -> &StoreState {
        &self.0.store
    }

    pub(super) fn with_state<R>(&self, f: impl FnOnce(&RefCell<State>) -> R) -> R {
        f(self.0.cold.get_or_init(|| RefCell::new(State::default())))
    }

    fn has_captures(&self) -> bool {
        self.0
            .cold
            .get()
            .is_some_and(|cold| !cold.borrow().captures.is_empty())
    }

    fn observe_scopes(&self, active: bool) {
        self.0.store.observe.set(active);
        if active {
            self.0.store.policy.set(JournalPolicy::JournalAndObserve);
        } else if self.0.store.policy.get() == JournalPolicy::JournalAndObserve {
            self.0.store.policy.set(JournalPolicy::JournalOnly);
        }
    }

    /// The first heap may adopt a pending capture made before any allocation
    /// heap existed. Once bound, a state always belongs to that one World.
    pub(crate) fn for_heap() -> Self {
        STATE.with(|slot| {
            let current = slot.owner.borrow();
            if !current.0.bound.replace(true) {
                return current.clone();
            }
            let next = Self::default();
            next.0.bound.set(true);
            next
        })
    }

    pub(crate) fn root(&self) -> NonNull<StoreState> {
        // Rc never exposes a unique borrow of this allocation. Shared access
        // retains provenance for every Cell, including later root copies.
        NonNull::from(&self.0.store)
    }

    pub(crate) fn publish_dump_window(&self, dump: BarrierWindow) {
        self.0.dump.set(dump);
    }

    pub(crate) fn install(&self, dump: BarrierWindow) {
        STATE.with(|slot| {
            if !Rc::ptr_eq(&slot.owner.borrow().0, &self.0) {
                let previous = slot.owner.borrow().clone();
                let previous_dump = previous.0.dump.get();
                previous.observe_scopes(previous.has_captures());
                if previous.has_captures()
                    && !slot
                        .suspended
                        .borrow()
                        .iter()
                        .any(|owner| Rc::ptr_eq(&owner.0, &previous.0))
                {
                    slot.suspended.borrow_mut().push(previous);
                }
                // A changed dump span discards negative protocol refinements,
                // as the old thread-local envelope did. Same-span round trips
                // retain valid refinements, including Off/Observed toggles.
                // Keep positive dependencies and open captures with their
                // World, so a temporary switch and return preserves them.
                if previous_dump != dump
                    && let Some(cold) = self.0.cold.get()
                {
                    cold.borrow_mut().observed.discard_exclusions();
                }
                *slot.owner.borrow_mut() = self.clone();
                slot.store.set(self.root());
                reset_recent_reads_for_heap_installation();
            }
        });
        self.publish_dump_window(dump);
        refresh_membership();
    }
}

// Preserve the original TLS allocation and pointer-slot displacement until
// the explicitly separate TLS-removal experiment. No reserve is accessed.
#[repr(C)]
struct LegacyStateLayout {
    captures: Vec<Capture>,
    writes: Box<[usize; JOURNAL_SIZE]>,
    policy: JournalPolicy,
    observed: observed_envelope::ObservedEnvelope,
}

#[repr(C)]
pub(super) struct StateSlot {
    owner: RefCell<ObservationWorld>,
    _prefix: [MaybeUninit<usize>; 2],
    store: Cell<NonNull<StoreState>>,
    // An installation-only round trip preserves an open capture. Actual
    // foreign projections reject it in the cold observer instead. This list
    // fits inside the existing TLS reserve and is never read by a hot store.
    suspended: RefCell<Vec<ObservationWorld>>,
    _reserve: [MaybeUninit<u8>; size_of::<RefCell<LegacyStateLayout>>() - 9 * size_of::<usize>()],
}

const _: () = {
    assert!(size_of::<StateSlot>() == size_of::<RefCell<LegacyStateLayout>>());
    assert!(align_of::<StateSlot>() == align_of::<RefCell<LegacyStateLayout>>());
    assert!(offset_of!(StateSlot, store) == 4 * size_of::<usize>());
    assert!(JOURNAL_SIZE.is_power_of_two());
};

impl Default for StateSlot {
    fn default() -> Self {
        let owner = ObservationWorld::default();
        let store = Cell::new(owner.root());
        Self {
            owner: RefCell::new(owner),
            _prefix: [MaybeUninit::uninit(); 2],
            store,
            suspended: RefCell::new(Vec::new()),
            _reserve: [MaybeUninit::uninit();
                size_of::<RefCell<LegacyStateLayout>>() - 9 * size_of::<usize>()],
        }
    }
}

/// One TLS pointer load and no RefCell borrow. The TLS owner retains the root
/// for this entire synchronous noncalling store operation.
#[inline(always)]
pub(crate) fn with_store<R>(f: impl FnOnce(&StoreState) -> R) -> R {
    STATE.with(|slot| {
        // SAFETY: slot.owner retains the allocation; installation updates the
        // root synchronously before mutation resumes. No store can hand off.
        f(unsafe { slot.store.get().as_ref() })
    })
}

/// Clone lifetime before borrowing cold data. No TLS-owner borrow is held
/// across cold publication, and the callback cannot return a borrowed State.
pub(super) fn with_state<R>(f: impl FnOnce(&RefCell<State>) -> R) -> R {
    ObservationWorld::current().with_state(f)
}

pub(crate) fn current_store_root() -> NonNull<StoreState> {
    STATE.with(|slot| slot.store.get())
}

pub(super) fn carried_placeholder() -> Box<dyn std::any::Any> {
    Box::new(ObservationWorld::default())
}

pub(super) fn swap_carried(slot: &mut dyn std::any::Any) {
    let arriving = slot
        .downcast_mut::<ObservationWorld>()
        .expect("carried World observation owner");
    // Foreign local captures do not migrate with this World. Crossing an OS
    // thread boundary cannot retain their unrecorded dependency projections.
    reject_foreign_capture_reads();
    STATE.with(|slot| {
        let departing = slot.owner.borrow().clone();
        departing.observe_scopes(departing.has_captures());
        std::mem::swap(&mut *slot.owner.borrow_mut(), arriving);
        let owner = slot.owner.borrow();
        slot.store.set(owner.root());
    });
    refresh_membership();
    reset_recent_reads_for_heap_installation();
}

/// Restore the existing private membership mirror after local activation,
/// scope exit, or a carried transfer. Open scopes from other local Worlds
/// enter only cold observation paths; an ordinary idle store reads no TLS.
pub(super) fn refresh_membership() {
    STATE.with(|slot| {
        let current = slot.owner.borrow();
        let mut suspended = slot.suspended.borrow_mut();
        suspended.retain(|owner| !Rc::ptr_eq(&owner.0, &current.0) && owner.has_captures());
        let active = current.has_captures() || !suspended.is_empty();
        current.observe_scopes(active);
        ACTIVE.with(|membership| membership.set(active));
    });
}

/// The current World's dependency map cannot describe a different World's
/// projection. Reject suspended foreign captures rather than certify a result
/// with missing reads. Merely installing another heap does not call this.
#[cold]
#[inline(never)]
pub(super) fn reject_foreign_capture_reads() {
    STATE.with(|slot| {
        for owner in slot.suspended.borrow().iter() {
            if let Some(cold) = owner.0.cold.get() {
                for capture in &mut cold.borrow_mut().captures {
                    capture.overflow = true;
                }
            }
        }
    });
}

pub(crate) fn clear_installed_store_heap(heap: *mut TaggedHeap) {
    // Teardown must not resurrect already-destroyed observation TLS.
    let _ = STATE.try_with(|slot| {
        // SAFETY: the slot's owning handle still retains this allocation.
        let store = unsafe { slot.store.get().as_ref() };
        if store.heap.get() == heap {
            store.heap.set(std::ptr::null_mut());
            store.publish_windows(BarrierWindow::NONE, BarrierWindow::NONE);
        }
    });
}
