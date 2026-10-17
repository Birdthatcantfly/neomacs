//! Cold cons catalog ownership for admitted mutators.
//!
//! The serial heap is an inert shell while this catalog owns its cons spine.
//! Only disjoint raw cell grants leave the gate: neither a heap reference nor
//! a vector/header reference is retained by a running owner. Collection first
//! closes every region and restores the spine under stopped authority.

use super::card_table::CardSlots;
use super::chunk_map::{ChunkClass, ChunkEntry, ChunkMap};
use super::cons_allocator::{ConsAllocatorOwner, ConsBlockBase};
use super::cons_blocks::ConsBlock;
use super::mutator_registration::{MutatorKey, RegistrationError};
use super::young_registry::YoungBlockLog;
use super::{ConsCell, HeapIdentity, TaggedHeap};
use rustc_hash::FxHashMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

const MAX_GRANT: usize = 128;

struct CatalogIdentity {
    abandoned: AtomicBool,
}

/// Distinguishes refill diagnostics without exposing a catalog index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CatalogSource {
    FreeList,
    BlockTail,
    FreshBlock,
}

/// Confined region; forgetting it leaves the catalog's active count charged.
/// Dropping it unclosed marks the catalog terminal and retains its backing.
#[must_use = "close the region through its original catalog and owner"]
pub(super) struct CatalogRegion {
    identity: Arc<CatalogIdentity>,
    owner: MutatorKey,
    first: NonNull<ConsCell>,
    total: usize,
    used: usize,
    source: CatalogSource,
    active: bool,
    _thread: PhantomData<Rc<()>>,
}
impl std::fmt::Debug for CatalogRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogRegion")
            .field("owner", &self.owner)
            .field("total", &self.total)
            .field("used", &self.used)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}
impl CatalogRegion {
    pub(super) fn total(&self) -> usize {
        self.total
    }
    pub(super) fn unused(&self) -> usize {
        self.total - self.used
    }
    pub(super) fn empty(&self) -> bool {
        self.used == self.total
    }
    pub(super) fn source(&self) -> CatalogSource {
        self.source
    }
    #[cfg(test)]
    pub(super) fn bounds_for_test(&self) -> (usize, usize, usize) {
        // SAFETY: the exclusive run includes every cell through total.
        let end = unsafe { self.first.as_ptr().add(self.total) };
        (self.first.as_ptr().addr(), end.addr(), self.unused())
    }

    /// # Safety
    /// The caller holds the matching live owner's run authority, initializes
    /// the complete returned ConsCell, and roots its value before ending that
    /// operation. No Lisp, safepoint, collection, or cross-owner access may
    /// occur during initialization. Cells are white/young in this proof era.
    pub(super) unsafe fn take_cell(&mut self) -> Option<*mut ConsCell> {
        if self.empty() || self.identity.abandoned.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: grant bounds this exclusive run within one retained block.
        // Preserve its original allocation provenance through pointer.add.
        let cell = unsafe { self.first.as_ptr().add(self.used) };
        self.used += 1;
        Some(cell)
    }
}
impl Drop for CatalogRegion {
    fn drop(&mut self) {
        if self.active {
            self.identity.abandoned.store(true, Ordering::Release);
        }
    }
}
static_assertions::assert_not_impl_any!(CatalogRegion: Send, Sync, Clone, Copy);

struct CatalogState {
    allocator: ConsAllocatorOwner,
    blocks: Vec<ConsBlock>,
    by_base: FxHashMap<usize, usize>,
    cards: Option<CardSlots>,
    chunk_map: Option<Arc<ChunkMap>>,
    live_count: usize,
    active_regions: usize,
    generational: bool,
    /// Detect replacement of the inert shell's unique placeholder allocator.
    placeholder: usize,
    #[cfg(test)]
    source_grants: [usize; 3],
}
// SAFETY: ownership of block allocations moves with this state; raw pointers
// are not Rust references into another owner's cells. The catalog mutex guards
// every spine/cursor mutation and the admitted allocator guards free links.
// Outside the gate only exclusive CatalogRegions can touch their own cells.
// Storage is never freed until stopped restoration or is retained on failure.
unsafe impl Send for CatalogState {}

/// Era-local catalog identity. This is not a cloneable heap facade.
/// The canonical registry validates keys/log binding before calling grant.
/// No method takes the registry, root-table, World, or worker-registry lock.
pub(super) struct ConsCatalog {
    heap: HeapIdentity,
    identity: Arc<CatalogIdentity>,
    state: Mutex<Option<CatalogState>>,
}
impl std::fmt::Debug for ConsCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsCatalog")
            .field("heap", &self.heap)
            .field(
                "abandoned",
                &self.identity.abandoned.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}
static_assertions::assert_impl_all!(ConsCatalog: Send, Sync);
static_assertions::assert_not_impl_any!(ConsCatalog: Clone, Copy);

impl ConsCatalog {
    #[cold]
    #[inline(never)]
    pub(super) fn new(heap: HeapIdentity) -> Self {
        Self {
            heap,
            identity: Arc::new(CatalogIdentity {
                abandoned: AtomicBool::new(false),
            }),
            state: Mutex::new(None),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, Option<CatalogState>>, RegistrationError> {
        if self.identity.abandoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        let guard = match self.state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                self.identity.abandoned.store(true, Ordering::Release);
                return Err(RegistrationError::Poisoned);
            }
        };
        if self.identity.abandoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        Ok(guard)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn active_regions(&self) -> Result<usize, RegistrationError> {
        Ok(self
            .lock()?
            .as_ref()
            .map_or(0, |state| state.active_regions))
    }

    /// # Safety
    /// The caller owns this exact quiescent serial heap, has stopped every
    /// participant with all regions closed, excludes marker launches/reentry,
    /// and suspends the whole-heap loan before any owner runs. While installed
    /// no other code may read/write the heap's inert cons placeholder fields.
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn install(&self, heap: &mut TaggedHeap) -> Result<(), RegistrationError> {
        if heap.heap_identity() != self.heap {
            return Err(RegistrationError::WrongHeap);
        }
        let mut gate = self.lock()?;
        if gate.is_some() {
            return Err(RegistrationError::Busy);
        }
        if heap.alloc_regions_open() {
            return Err(RegistrationError::OpenRegion);
        }
        if heap.concurrent_mark_running
            || heap.mark_in_progress
            || heap.sweep_in_progress
            || heap.generational.major_in_progress
        {
            return Err(RegistrationError::Busy);
        }
        let placeholder = ConsAllocatorOwner::new(self.heap);
        if !heap.cons_allocator.threads_admitted() {
            // SAFETY: inherited from install's stopped/closed storage contract.
            unsafe { heap.admit_threads_world_stopped() };
        }
        let allocator = std::mem::replace(&mut heap.cons_allocator, placeholder);
        let placeholder =
            (&*heap.cons_allocator as *const super::cons_allocator::ConsAllocator) as usize;
        let state = CatalogState {
            allocator,
            blocks: std::mem::take(&mut heap.cons_blocks),
            by_base: std::mem::take(&mut heap.cons_block_index_by_base),
            cards: heap.card_slots_mut().map(std::mem::take),
            chunk_map: heap.chunk_map.as_ref().map(|map| Arc::clone(map.shared())),
            live_count: std::mem::take(&mut heap.cons_live_count),
            active_regions: 0,
            generational: heap.generational.enabled,
            placeholder,
            #[cfg(test)]
            source_grants: [0; 3],
        };
        *gate = Some(state);
        Ok(())
    }

    /// # Safety
    /// The matching all-stopped transition excludes every running/initializing
    /// owner, worker and callback. All owner regions are closed. The inert heap
    /// placeholders have not been accessed or changed since install.
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn restore(&self, heap: &mut TaggedHeap) -> Result<(), RegistrationError> {
        if heap.heap_identity() != self.heap {
            return Err(RegistrationError::WrongHeap);
        }
        let mut gate = self.lock()?;
        let state = gate.as_ref().ok_or(RegistrationError::Publication)?;
        if state.active_regions != 0 {
            return Err(RegistrationError::OpenRegion);
        }
        let placeholder =
            (&*heap.cons_allocator as *const super::cons_allocator::ConsAllocator) as usize;
        let same_map = match (&state.chunk_map, &heap.chunk_map) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b.shared()),
            _ => false,
        };
        if placeholder != state.placeholder
            || !heap.cons_blocks.is_empty()
            || heap.cons_blocks.capacity() != 0
            || !heap.cons_block_index_by_base.is_empty()
            || heap.cons_block_index_by_base.capacity() != 0
            || heap.cons_live_count != 0
            || heap.cons_allocator.threads_admitted()
            || !heap.cons_allocator.free_counts().is_empty()
            || heap.generational.enabled != state.generational
            || !same_map
            || heap.card_slots().is_some() != state.cards.is_some()
            || heap.card_slots().is_some_and(|slots| !slots.is_empty())
        {
            return Err(RegistrationError::Publication);
        }
        let state = gate.take().expect("validated installed catalog");
        heap.cons_allocator = state.allocator;
        heap.cons_blocks = state.blocks;
        heap.cons_block_index_by_base = state.by_base;
        heap.cons_live_count = state.live_count;
        if let Some(cards) = state.cards {
            *heap.card_slots_mut().expect("validated card carrier") = cards;
        }
        Ok(())
    }

    /// The caller's canonical registry has validated the full key and its
    /// uniquely borrowed resident young payload. No raw storage is usable
    /// until all catalog metadata, young recording and charging have committed.
    #[cold]
    #[inline(never)]
    pub(super) fn grant(
        &self,
        owner: MutatorKey,
        young: &mut YoungBlockLog,
        want: usize,
    ) -> Result<CatalogRegion, RegistrationError> {
        if owner.heap_identity() != self.heap || young.id().heap_identity() != self.heap {
            return Err(RegistrationError::WrongHeap);
        }
        if !(1..=MAX_GRANT).contains(&want) {
            return Err(RegistrationError::Capacity);
        }
        let mut gate = self.lock()?;
        let state = gate.as_mut().ok_or(RegistrationError::Publication)?;
        state
            .live_count
            .checked_add(want)
            .ok_or(RegistrationError::Exhausted)?;
        let active = state
            .active_regions
            .checked_add(1)
            .ok_or(RegistrationError::Exhausted)?;
        // SAFETY: this catalog owns every registered block, no sweep overlaps,
        // and the existing admitted allocator removes an exclusive free run.
        let free = unsafe { state.allocator.reserve_run(want) };
        let (first, count, source) = if let Some((first, count)) = free {
            (first, count, CatalogSource::FreeList)
        } else {
            let tail = state
                .blocks
                .last_mut()
                .filter(|block| state.allocator.is_ready(ConsBlockBase::of(block)))
                .and_then(|block| {
                    let (start, count) = block.reserve_tail(want);
                    (count != 0).then(|| {
                        // SAFETY: reserve_tail bounds this run in owned storage.
                        (unsafe { block.cells_ptr().add(start) }, count)
                    })
                });
            if let Some((first, count)) = tail {
                (first, count, CatalogSource::BlockTail)
            } else {
                // Install retained storage ownership before metadata can name
                // it. Any unwind after this point poisons the gate; Drop keeps
                // the entire installed catalog, including this backing alive.
                let mut block = ConsBlock::new();
                let (_, count) = block.reserve_tail(want);
                let first = block.cells_ptr();
                let base = block.base_addr();
                state.blocks.push(block);
                let index = state.blocks.len() - 1;
                state.allocator.register(&state.blocks[index]);
                assert!(
                    state.by_base.insert(base, index).is_none(),
                    "duplicate cons base"
                );
                if state.generational {
                    if let Some(cards) = &mut state.cards {
                        cards.insert_block(base);
                    }
                }
                // Release publication is LAST: a reader must never discover a
                // chunk entry before allocator/vector/base/card registration.
                if let Some(map) = &state.chunk_map {
                    map.set(base, ChunkEntry::new(ChunkClass::Cons, index));
                }
                (first, count, CatalogSource::FreshBlock)
            }
        };
        let base = ConsBlock::block_base_for_ptr(first);
        let index = *state
            .by_base
            .get(&base)
            .expect("grant has canonical base index");
        if state.generational {
            if let Err(error) = state
                .allocator
                .note_carve(ConsBlockBase::of(&state.blocks[index]), young)
            {
                // No marks, initialization, charge or handout occurred. The
                // reserved run remains uniquely ours in retained storage.
                unsafe { state.allocator.return_unused(first, count) };
                return Err(error.into());
            }
        }
        state.live_count += count;
        state.active_regions = active;
        #[cfg(test)]
        {
            let source_index = match source {
                CatalogSource::FreeList => 0,
                CatalogSource::BlockTail => 1,
                CatalogSource::FreshBlock => 2,
            };
            state.source_grants[source_index] += 1;
        }
        Ok(CatalogRegion {
            identity: Arc::clone(&self.identity),
            owner,
            first: NonNull::new(first).expect("nonempty cons run"),
            total: count,
            used: 0,
            source,
            active: true,
            _thread: PhantomData,
        })
    }

    #[cold]
    #[inline(never)]
    pub(super) fn close(
        &self,
        owner: MutatorKey,
        mut region: CatalogRegion,
    ) -> Result<usize, (RegistrationError, CatalogRegion)> {
        let result = (|| {
            if !Arc::ptr_eq(&self.identity, &region.identity) {
                return Err(RegistrationError::StaleRegistration);
            }
            if owner != region.owner {
                return Err(RegistrationError::WrongOwner);
            }
            let mut gate = self.lock()?;
            let state = gate.as_mut().ok_or(RegistrationError::Publication)?;
            if !region.active || state.active_regions == 0 {
                return Err(RegistrationError::OutstandingLoan);
            }
            let unused = region.unused();
            let base = ConsBlock::block_base_for_ptr(region.first.as_ptr());
            let index = *state
                .by_base
                .get(&base)
                .ok_or(RegistrationError::Publication)?;
            let block = &mut state.blocks[index];
            if block.base_addr() != base || state.live_count < unused {
                return Err(RegistrationError::Publication);
            }
            // SAFETY: only the unhanded suffix is touched; pointer provenance
            // comes from the original grant, and no stopped sweep overlaps.
            let first = unsafe { region.first.as_ptr().add(region.used) };
            let start = ConsBlock::index_of_ptr(first);
            if unused != 0 {
                ConsBlock::mark_run_at(base, start, unused, false);
                if region.source != CatalogSource::FreeList
                    && block.next_index as usize == start + unused
                {
                    block.rewind_tail(start);
                } else {
                    // SAFETY: these exact unused young cells are absent from
                    // every free list and exclusively owned by this region.
                    unsafe { state.allocator.return_unused(first, unused) };
                }
                state.live_count -= unused;
            }
            state.active_regions -= 1;
            region.active = false;
            Ok(unused)
        })();
        result.map_err(|error| (error, region))
    }
}
/// Counts/addresses only; never an unlocked storage borrow.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CatalogSnapshot {
    pub(super) installed: bool,
    pub(super) blocks: Vec<(usize, usize)>,
    pub(super) metadata_coherent: bool,
    pub(super) free_count: usize,
    pub(super) active_regions: usize,
    pub(super) live_count: usize,
    /// Free-list, existing-tail, fresh-block grants, respectively.
    pub(super) source_grants: [usize; 3],
}
#[cfg(test)]
impl ConsCatalog {
    pub(super) fn snapshot_for_test(&self) -> Result<CatalogSnapshot, RegistrationError> {
        let gate = self.lock()?;
        let Some(state) = gate.as_ref() else {
            return Ok(CatalogSnapshot {
                installed: false,
                blocks: Vec::new(),
                metadata_coherent: true,
                free_count: 0,
                active_regions: 0,
                live_count: 0,
                source_grants: [0; 3],
            });
        };
        let free = state.allocator.free_counts();
        let coherent = state.blocks.len() == state.by_base.len()
            && state.blocks.len() == free.len()
            && state.blocks.iter().enumerate().all(|(index, block)| {
                let base = block.base_addr();
                state.by_base.get(&base) == Some(&index)
                    && free.iter().any(|(key, count)| {
                        key.addr() == base && *count <= block.next_index as usize
                    })
                    && state.chunk_map.as_ref().is_none_or(|map| {
                        let entry = map.get(base);
                        entry.is(ChunkClass::Cons) && entry.index() == index
                    })
                    && (!state.generational
                        || state.cards.as_ref().is_none_or(|cards| {
                            cards
                                .blocks(super::card_table::ChunkSlot::of(base))
                                .contains(&base)
                        }))
            });
        Ok(CatalogSnapshot {
            installed: true,
            blocks: state
                .blocks
                .iter()
                .enumerate()
                .map(|(i, b)| (b.base_addr(), i))
                .collect(),
            metadata_coherent: coherent,
            free_count: free.iter().map(|(_, n)| n).sum(),
            active_regions: state.active_regions,
            live_count: state.live_count,
            source_grants: state.source_grants,
        })
    }
}

impl Drop for ConsCatalog {
    fn drop(&mut self) {
        self.identity.abandoned.store(true, Ordering::Release);
        // A forgotten/abandoned era cannot establish serial ownership. Even
        // zero active regions may contain rooted cells. Only explicit restore
        // can release this ownership into the original heap; retain otherwise.
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(state) = state.take() {
            std::mem::forget(state);
        }
    }
}
