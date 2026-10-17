//! Cold canonical registration. This module grants no heap execution or trace
//! authority. The serial inline iterator remains the Sim representation.
//!
//! An era owns the actual World Baton and the quiescent heap loan. Only a
//! metadata permit crosses a thread boundary; owner buffers are built there.
//! Unfinished teardown retains both authority and backing instead of returning
//! a usable serial World after an abandoned owner.
#![allow(dead_code)] // Consumers are admitted in subsequent proof batches.

mod image;
mod summaries;
mod token;

pub use summaries::{RegistrationCensus, StoppedSummaries};
pub use token::{
    Mutator, MutatorFinishFailure, MutatorPermit, MutatorStartFailure, OwnerPublication, RunAccess,
    YoungWorkerLoan,
};

use std::cell::Cell;
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;

use rustc_hash::FxHashMap;

use super::young_registry::{YoungRegistrationId, YoungRegistryError};
use super::{HeapIdentity, QuiescentCollector};
use crate::emacs_core::eval::world::Baton;
use image::{OwnerGcState, PublishedGcImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationError {
    WrongHeap,
    WrongOwner,
    StaleRegistration,
    DuplicateOwner,
    WrongThread,
    Poisoned,
    Busy,
    Exhausted,
    Capacity,
    OutstandingLoan,
    OpenRegion,
    ResidualSatb,
    Publication,
    PendingRoots,
    PendingYoung,
}

impl std::fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mutator registration refused: {self:?}")
    }
}
impl std::error::Error for RegistrationError {}

impl From<YoungRegistryError> for RegistrationError {
    fn from(error: YoungRegistryError) -> Self {
        match error {
            YoungRegistryError::WrongHeap => Self::WrongHeap,
            #[cfg(test)]
            YoungRegistryError::RegistrationExhausted => Self::Exhausted,
            _ => Self::OutstandingLoan,
        }
    }
}

// Heap/owner pairs and local registration ordinals can recur in a later era.
// A checked generation never does, including for the frozen legacy entry.
static NEXT_REGISTRATION_GENERATION: AtomicU64 = AtomicU64::new(1);

fn issue_generation(counter: &AtomicU64) -> Result<NonZeroU64, RegistrationError> {
    let issued = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .map_err(|_| RegistrationError::Exhausted)?;
    NonZeroU64::new(issued).ok_or(RegistrationError::Exhausted)
}

/// Copyable identity, never execution or World authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MutatorKey {
    heap: HeapIdentity,
    world: HeapIdentity,
    owner: u64,
    registration: NonZeroU64,
    generation: NonZeroU64,
}

impl MutatorKey {
    pub fn heap_identity(self) -> HeapIdentity {
        self.heap
    }
    pub fn owner(self) -> u64 {
        self.owner
    }
    pub fn generation(self) -> NonZeroU64 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryPhase {
    Frozen,
    Starting,
    Running,
    Published,
    Retiring,
    Retired,
}

struct EntryState {
    phase: EntryPhase,
    owner_thread: Option<ThreadId>,
    publication_generation: u64,
    epoch: u64,
    image: Option<PublishedGcImage>,
    run_loan: bool,
    worker_loan: bool,
    readers: usize,
}

/// Stable Send + Sync metadata. It never contains a Context, heap reference,
/// owner-state borrow, or owner-confined SATB buffer.
pub struct MutatorEntry {
    key: MutatorKey,
    young: YoungRegistrationId,
    cancelled_start: AtomicBool,
    state: Mutex<EntryState>,
}

impl std::fmt::Debug for MutatorEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutatorEntry")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl MutatorEntry {
    pub fn key(&self) -> MutatorKey {
        self.key
    }
}

struct RegistryState {
    entries: Vec<Arc<MutatorEntry>>,
    // An index into the SAME entry set, never another payload/owner registry.
    young_index: FxHashMap<YoungRegistrationId, usize>,
    next_registration: u64,
    generation_limit: u64,
}

struct Registry {
    heap: HeapIdentity,
    world: HeapIdentity,
    state: Mutex<RegistryState>,
    poisoned: AtomicBool,
}

impl Registry {
    fn lock(&self) -> Result<MutexGuard<'_, RegistryState>, RegistrationError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        match self.state.try_lock() {
            Ok(gate) => {
                if self.poisoned.load(Ordering::Acquire) {
                    Err(RegistrationError::Poisoned)
                } else {
                    Ok(gate)
                }
            }
            Err(std::sync::TryLockError::WouldBlock) => Err(RegistrationError::Busy),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                self.poison();
                Err(RegistrationError::Poisoned)
            }
        }
    }

    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }

    fn validate(
        &self,
        state: &RegistryState,
        key: MutatorKey,
    ) -> Result<Arc<MutatorEntry>, RegistrationError> {
        if key.heap != self.heap || key.world != self.world {
            return Err(RegistrationError::WrongHeap);
        }
        let index = usize::try_from(key.registration.get() - 1)
            .map_err(|_| RegistrationError::StaleRegistration)?;
        let entry = state
            .entries
            .get(index)
            .ok_or(RegistrationError::StaleRegistration)?;
        if entry.key.owner != key.owner {
            return Err(RegistrationError::WrongOwner);
        }
        if entry.key != key {
            return Err(RegistrationError::StaleRegistration);
        }
        Ok(Arc::clone(entry))
    }
}

/// Keeper-confined RAII admission, with both heap and World retained.
/// There is deliberately no Deref, heap accessor, or constructor from IDs.
#[must_use = "finish explicitly; abandonment retains the World and poisons the era"]
pub struct ParallelHeapEra<'heap> {
    collector: Option<QuiescentCollector<'heap>>,
    keeper: Option<Baton>,
    registry: Option<Arc<Registry>>,
    _thread: PhantomData<Rc<()>>,
}

pub(crate) struct EraAdmissionFailure<'heap> {
    pub(crate) reason: RegistrationError,
    pub(crate) collector: QuiescentCollector<'heap>,
    pub(crate) keeper: Baton,
}

impl std::fmt::Debug for EraAdmissionFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EraAdmissionFailure")
            .field(&self.reason)
            .finish()
    }
}
impl std::fmt::Debug for ParallelHeapEra<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelHeapEra")
            .field("heap", &self.registry.as_ref().map(|r| r.heap))
            .finish_non_exhaustive()
    }
}

/// Busy/error recovery returns the entire admission; it never restores serial
/// authority merely because one owner is delayed.
pub(crate) struct EraFinishFailure<'heap> {
    pub(crate) reason: RegistrationError,
    pub(crate) era: ParallelHeapEra<'heap>,
}
impl std::fmt::Debug for EraFinishFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EraFinishFailure")
            .field(&self.reason)
            .finish()
    }
}

impl<'heap> ParallelHeapEra<'heap> {
    #[cold]
    #[inline(never)]
    pub(crate) fn admit(
        mut collector: QuiescentCollector<'heap>,
        keeper: Baton,
    ) -> Result<Self, EraAdmissionFailure<'heap>> {
        let heap = collector.heap_identity();
        let reason = if heap != keeper.parallel_heap_identity() {
            Some(RegistrationError::WrongHeap)
        } else if !keeper.held_here() {
            Some(RegistrationError::WrongThread)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(EraAdmissionFailure {
                reason,
                collector,
                keeper,
            });
        }
        let generation = match issue_generation(&NEXT_REGISTRATION_GENERATION) {
            Ok(generation) => generation,
            Err(reason) => {
                return Err(EraAdmissionFailure {
                    reason,
                    collector,
                    keeper,
                });
            }
        };
        let image = match PublishedGcImage::from_legacy(collector.registration_heap_mut()) {
            Ok(image) => image,
            Err(error) => {
                return Err(EraAdmissionFailure {
                    reason: error.into(),
                    collector,
                    keeper,
                });
            }
        };
        let young = image.young_id();
        let key = MutatorKey {
            heap,
            world: heap,
            owner: 0,
            registration: NonZeroU64::MIN,
            generation,
        };
        let entry = Arc::new(MutatorEntry {
            key,
            young,
            cancelled_start: AtomicBool::new(false),
            state: Mutex::new(EntryState {
                phase: EntryPhase::Frozen,
                owner_thread: Some(std::thread::current().id()),
                publication_generation: 1,
                epoch: 0,
                image: Some(image),
                run_loan: false,
                worker_loan: false,
                readers: 0,
            }),
        });
        let mut young_index = FxHashMap::default();
        young_index.insert(young, 0);
        Ok(Self {
            collector: Some(collector),
            keeper: Some(keeper),
            registry: Some(Arc::new(Registry {
                heap,
                world: heap,
                poisoned: AtomicBool::new(false),
                state: Mutex::new(RegistryState {
                    entries: vec![entry],
                    young_index,
                    next_registration: 2,
                    generation_limit: u64::MAX,
                }),
            })),
            _thread: PhantomData,
        })
    }

    fn registry(&self) -> &Registry {
        self.registry.as_ref().expect("live era")
    }

    pub fn heap_identity(&self) -> HeapIdentity {
        self.registry().heap
    }

    #[cold]
    #[inline(never)]
    pub fn register(
        &self,
        owner: u64,
        root_capacity: usize,
    ) -> Result<MutatorPermit<'_>, RegistrationError> {
        if owner == 0 {
            return Err(RegistrationError::WrongOwner);
        }
        let registry = self.registry();
        let mut state = registry.lock()?;
        for entry in &state.entries {
            if entry.key.owner == owner && !entry.cancelled_start.load(Ordering::Acquire) {
                let current = entry
                    .state
                    .lock()
                    .map_err(|_| RegistrationError::Poisoned)?;
                if current.phase != EntryPhase::Retired {
                    return Err(RegistrationError::DuplicateOwner);
                }
            }
        }
        let next = state.next_registration;
        let following = next
            .checked_add(1)
            .filter(|n| *n <= state.generation_limit)
            .ok_or(RegistrationError::Exhausted)?;
        let registration = NonZeroU64::new(next).ok_or(RegistrationError::Exhausted)?;
        state
            .entries
            .try_reserve(1)
            .map_err(|_| RegistrationError::Capacity)?;
        state
            .young_index
            .try_reserve(1)
            .map_err(|_| RegistrationError::Capacity)?;
        let young = YoungRegistrationId::issue_parallel(registry.heap)
            .map_err(|_| RegistrationError::Exhausted)?;
        let generation = issue_generation(&NEXT_REGISTRATION_GENERATION)?;
        let key = MutatorKey {
            heap: registry.heap,
            world: registry.world,
            owner,
            registration,
            generation,
        };
        let entry = Arc::new(MutatorEntry {
            key,
            young,
            cancelled_start: AtomicBool::new(false),
            state: Mutex::new(EntryState {
                phase: EntryPhase::Starting,
                owner_thread: None,
                publication_generation: 0,
                epoch: 0,
                image: None,
                run_loan: false,
                worker_loan: false,
                readers: 0,
            }),
        });
        let index = state.entries.len();
        state.young_index.insert(young, index);
        state.entries.push(Arc::clone(&entry));
        state.next_registration = following;
        Ok(MutatorPermit {
            registry,
            entry,
            root_capacity,
            armed: true,
            _unshared: PhantomData,
        })
    }

    pub fn stopped_summaries(&self) -> Result<StoppedSummaries<'_>, RegistrationError> {
        StoppedSummaries::new(self.registry())
    }

    /// Explicit teardown is metadata-only. No collection, polling, or waits.
    /// New young backlinks and result roots must first be discharged by the
    /// later collector/root authority; A3 refuses them conservatively.
    pub(crate) fn finish(
        mut self,
    ) -> Result<(QuiescentCollector<'heap>, Baton), EraFinishFailure<'heap>> {
        if let Err(reason) = self.prepare_finish() {
            return Err(EraFinishFailure { reason, era: self });
        }
        let collector = self.collector.take().expect("live era");
        let keeper = self.keeper.take().expect("live era");
        self.registry.take();
        Ok((collector, keeper))
    }

    fn prepare_finish(&mut self) -> Result<(), RegistrationError> {
        let registry = self.registry.as_ref().expect("live era");
        let state = registry.lock()?;
        // Preflight EVERY entry before moving any image, including retired
        // retained entries. Cancelled starts never became runnable.
        for (index, entry) in state.entries.iter().enumerate() {
            if entry.cancelled_start.load(Ordering::Acquire) {
                continue;
            }
            let current = entry
                .state
                .lock()
                .map_err(|_| RegistrationError::Poisoned)?;
            if current.run_loan || current.worker_loan || current.readers != 0 {
                return Err(RegistrationError::OutstandingLoan);
            }
            if (index == 0 && current.phase != EntryPhase::Frozen)
                || (index != 0 && current.phase != EntryPhase::Retiring)
            {
                return Err(RegistrationError::Busy);
            }
            let image = current
                .image
                .as_ref()
                .ok_or(RegistrationError::Publication)?;
            if !image.roots_empty() {
                return Err(RegistrationError::PendingRoots);
            }
            if index != 0 && !image.young_empty() {
                return Err(RegistrationError::PendingYoung);
            }
        }
        let mut legacy = state.entries[0]
            .state
            .lock()
            .map_err(|_| RegistrationError::Poisoned)?;
        let mut image = legacy.image.take().expect("preflighted frozen image");
        for entry in state.entries.iter().skip(1) {
            if entry.cancelled_start.load(Ordering::Acquire) {
                continue;
            }
            let mut current = entry
                .state
                .lock()
                .map_err(|_| RegistrationError::Poisoned)?;
            let other = current.image.take().expect("preflighted retiring image");
            if let Err(failure) = image.merge_non_young(other) {
                current.image = Some(failure.image);
                legacy.image = Some(image);
                registry.poison();
                return Err(failure.error.into());
            }
            current.phase = EntryPhase::Retired;
        }
        if let Err(failure) = image.restore_legacy(
            self.collector
                .as_mut()
                .expect("live era")
                .registration_heap_mut(),
        ) {
            legacy.image = Some(failure.image);
            registry.poison();
            return Err(failure.error.into());
        }
        legacy.phase = EntryPhase::Retired;
        Ok(())
    }
}

impl Drop for ParallelHeapEra<'_> {
    fn drop(&mut self) {
        let Some(registry) = self.registry.take() else {
            return;
        };
        registry.poison();
        // No mutex, collection, join, callback, or implicit serial resumption.
        // The leaked keeper retains World/heap backing and facade exclusion.
        std::mem::forget(registry);
        if let Some(keeper) = self.keeper.take() {
            std::mem::forget(keeper);
        }
    }
}

static_assertions::assert_not_impl_any!(ParallelHeapEra<'static>: Send, Sync, Clone, Copy);
static_assertions::assert_impl_all!(MutatorEntry: Send, Sync);
static_assertions::assert_impl_all!(MutatorKey: Send, Sync, Copy);

#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod tests;
