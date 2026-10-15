//! Shared, cold-path cons free-list allocator.
//!
//! Each registered block has its own intrusive list. Sole-owner access uses
//! direct borrows. Sticky thread admission moves all metadata behind one mutex.
//! Allocation regions are
//! disjoint after reservation and their owners fill cells without this lock.
//! Only its mutex-only SharedConsAllocator facade can cross threads; the
//! allocator owner and TaggedHeap remain confined.
//!
//! Storage stays owned by ConsBlock. Unsafe operations require that registered
//! storage remain alive, and that collection park all mutators and close their
//! regions before withholding/rebuilding lists. No cloneable storage handle is
//! created here. A caller must unregister before releasing a block.

use super::young_registry::{
    StoppedYoungLogs, YoungBlockIndex, YoungBlockLog, YoungRegistrationId, YoungRegistryError,
};
use super::*;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use typed_index_collections::TiVec;

/// Own the cold allocator without introducing a pointer niche into the inline
/// heap. A bare Box changes repr(Rust) field ordering and JIT offsets; the
/// Option's null representation preserves the former raw-pointer layout.
/// Construction always installs Some, and the field is private thereafter.
#[derive(Debug)]
#[repr(transparent)]
pub(super) struct ConsAllocatorOwner(Option<Box<ConsAllocator>>);

impl ConsAllocatorOwner {
    /// Reuse the existing heap identity; this never issues another domain.
    pub(super) fn new(identity: HeapIdentity) -> Self {
        Self(Some(Box::new(ConsAllocator::new(identity))))
    }
}

impl std::ops::Deref for ConsAllocatorOwner {
    type Target = ConsAllocator;
    fn deref(&self) -> &Self::Target {
        self.0.as_deref().expect("allocator owner is initialized")
    }
}

impl ConsAllocatorOwner {
    /// # Safety
    /// Same stopped-world storage contract as ConsAllocator::stopped_world.
    pub(super) unsafe fn stopped_world(&mut self) -> ConsStopAll<'_> {
        // SAFETY: forward the caller's documented storage/protocol contract
        // unchanged through this private exclusive owner borrow.
        unsafe {
            self.0
                .as_deref_mut()
                .expect("allocator owner is initialized")
                .stopped_world()
        }
    }

    pub(super) fn register(&mut self, block: &ConsBlock) {
        self.0
            .as_deref_mut()
            .expect("allocator owner is initialized")
            .register(block)
    }

    pub(super) fn unregister(&mut self, base: ConsBlockBase) {
        self.0
            .as_deref_mut()
            .expect("allocator owner is initialized")
            .unregister(base)
    }

    /// Record a region grant before its mutator can initialize any cell.
    /// The sole owner uses a unique plain borrow; admitted mutators hold the
    /// allocator mutex and deduplicate with a relaxed atomic swap. Only the
    /// false-to-true transition appends to the supplied canonical owner log.
    /// A wrong-heap log is rejected before any flag or log mutation.
    #[inline]
    pub(super) fn note_carve(
        &mut self,
        base: ConsBlockBase,
        log: &mut YoungBlockLog,
    ) -> Result<(), YoungRegistryError> {
        self.0
            .as_deref_mut()
            .expect("allocator owner is initialized")
            .note_carve(base, log)
    }

    /// # Safety
    /// Every free node must name live registered block storage, exclusively
    /// owned by this allocator until returned as a disjoint allocation run.
    /// Registration/storage lifetime must cover this call and the returned run.
    #[inline]
    pub(super) unsafe fn reserve_run(&mut self, want: usize) -> Option<(*mut ConsCell, usize)> {
        // SAFETY: forward the caller's documented storage/protocol contract
        // unchanged through this private exclusive owner borrow.
        unsafe {
            self.0
                .as_deref_mut()
                .expect("allocator owner is initialized")
                .reserve_run(want)
        }
    }

    /// # Safety
    /// The range consists of unhanded cells of one still-live registered block,
    /// owned exclusively by the caller and absent from every free list. Its
    /// marks/ages must already have been cleared. No collector may concurrently
    /// sweep this range. A withheld block stays unavailable until publication.
    #[inline]
    pub(super) unsafe fn return_unused(&mut self, first: *mut ConsCell, unused: usize) {
        // SAFETY: forward the caller's documented storage/protocol contract
        // unchanged through this private exclusive owner borrow.
        unsafe {
            self.0
                .as_deref_mut()
                .expect("allocator owner is initialized")
                .return_unused(first, unused)
        }
    }

    /// # Safety
    /// `head` links exactly the `next_index - live` dead cells of `block`, with
    /// no live/reserved cell included, and the block remains alive until removed.
    /// Rebuilding has exclusive access to those cells; publication is last.
    pub(super) unsafe fn publish(&mut self, block: &ConsBlock, head: *mut ConsCell, live: usize) {
        // SAFETY: forward the caller's documented storage/protocol contract
        // unchanged through this private exclusive owner borrow.
        unsafe {
            self.0
                .as_deref_mut()
                .expect("allocator owner is initialized")
                .publish(block, head, live)
        }
    }
}

static_assertions::assert_eq_size!(ConsAllocatorOwner, *mut ConsCell);
const _: () = assert!(align_of::<ConsAllocatorOwner>() == align_of::<*mut ConsCell>());

/// Stable identity unaffected by ConsBlock vector compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct ConsBlockBase(usize);

impl ConsBlockBase {
    pub(super) fn of(block: &ConsBlock) -> Self {
        Self(block.base_addr())
    }

    pub(super) fn addr(self) -> usize {
        self.0
    }

    fn of_cell(cell: *const ConsCell) -> Self {
        Self(ConsBlock::block_base_for_ptr(cell))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockAvailability {
    Ready,
    WithheldForSweep,
}

#[derive(Debug)]
struct BlockFreeState {
    base: ConsBlockBase,
    young: AtomicBool,
    young_log_slot: Option<YoungLogSlot>,
    head: usize,
    count: usize,
    available_slot: Option<usize>,
    availability: BlockAvailability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct YoungLogSlot {
    registration: YoungRegistrationId,
    index: YoungBlockIndex,
}

/// Private metadata identity, never exposed by the allocator facade or retained
/// beyond its exclusive metadata access. Unregister removes every stored reference
/// before reuse, so no escaping handle needs an incarnation or pointer cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
struct BlockSlot(NonZeroUsize);

impl BlockSlot {
    fn for_append(slots: &TiVec<Self, Option<BlockFreeState>>) -> Self {
        // A nonzero-sized Vec entry bounds length below usize::MAX; only this
        // private constructor creates a fresh index, at the append position.
        Self(NonZeroUsize::new(slots.len() + 1).expect("cons metadata slot fits usize"))
    }
}

impl From<BlockSlot> for usize {
    #[inline]
    fn from(slot: BlockSlot) -> Self {
        slot.0.get() - 1
    }
}

static_assertions::assert_eq_size!(BlockSlot, usize);
static_assertions::assert_eq_size!(Option<BlockSlot>, usize);
static_assertions::assert_impl_all!(BlockSlot: Send, Sync, Copy, std::fmt::Debug);

struct AllocatorState {
    /// Existing heap domain, shared with the canonical cold SATB registry.
    /// It is not a second admission bit or an independently issued identity.
    identity: HeapIdentity,
    blocks: FxHashMap<ConsBlockBase, BlockSlot>,
    slots: TiVec<BlockSlot, Option<BlockFreeState>>,
    vacant_slots: Vec<BlockSlot>,
    blocks_with_free: Vec<BlockSlot>,
    /// Work index ONLY. Canonical cold registrations own the actual log Vecs.
    pending_mutators: Vec<YoungRegistrationId>,
    mark_work: Vec<ConsBlockBase>,
}

impl std::fmt::Debug for AllocatorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllocatorState")
            .field("identity", &self.identity)
            .field("blocks", &self.blocks)
            .field("slots", &self.slots.raw)
            .field("vacant_slots", &self.vacant_slots)
            .field("blocks_with_free", &self.blocks_with_free)
            .field("pending_mutators", &self.pending_mutators)
            .field("mark_work", &self.mark_work)
            .finish()
    }
}

impl AllocatorState {
    fn new(identity: HeapIdentity) -> Self {
        Self {
            identity,
            blocks: FxHashMap::default(),
            slots: TiVec::new(),
            vacant_slots: Vec::new(),
            blocks_with_free: Vec::new(),
            pending_mutators: Vec::new(),
            mark_work: Vec::new(),
        }
    }

    fn ensure_young_registry(&self, logs: &StoppedYoungLogs<'_>) -> Result<(), YoungRegistryError> {
        if logs.heap_identity() == self.identity {
            Ok(())
        } else {
            Err(YoungRegistryError::WrongHeap)
        }
    }

    #[inline]
    fn block(&self, slot: BlockSlot) -> &BlockFreeState {
        self.slots
            .get(slot)
            .and_then(Option::as_ref)
            .expect("live cons metadata slot")
    }

    #[inline]
    fn block_mut(&mut self, slot: BlockSlot) -> &mut BlockFreeState {
        self.slots
            .get_mut(slot)
            .and_then(Option::as_mut)
            .expect("live cons metadata slot")
    }

    fn slot(&self, base: ConsBlockBase) -> BlockSlot {
        *self.blocks.get(&base).expect("registered cons block")
    }

    fn block_for_base(&self, base: ConsBlockBase) -> &BlockFreeState {
        self.block(self.slot(base))
    }

    fn block_for_base_mut(&mut self, base: ConsBlockBase) -> &mut BlockFreeState {
        let slot = self.slot(base);
        self.block_mut(slot)
    }

    fn verify_slots(&self) {
        let mut visited: TiVec<BlockSlot, bool> = vec![false; self.slots.len()].into();
        for &slot in &self.vacant_slots {
            assert!(!visited[slot], "duplicate vacant slot");
            visited[slot] = true;
            assert!(self.slots[slot].is_none(), "vacant slot is live");
        }
        for (base, &slot) in &self.blocks {
            assert!(!visited[slot], "slot reused by two block identities");
            visited[slot] = true;
            assert_eq!(self.block(slot).base, *base);
        }
        assert!(visited.iter().all(|&seen| seen), "untracked metadata slot");
    }

    /// Remove one canonical log entry in O(1), before the base map is removed
    /// or any storage can be reused. The stopped resolver excludes worker loans.
    fn unlink_young_log(
        &mut self,
        base: ConsBlockBase,
        logs: &mut StoppedYoungLogs<'_>,
    ) -> Result<(), YoungRegistryError> {
        self.ensure_young_registry(logs)?;
        let Some(slot) = self.blocks.get(&base).copied() else {
            return Ok(());
        };
        let Some(location) = self.block(slot).young_log_slot else {
            return Ok(());
        };
        let (removal, pending_removal) = {
            let log = logs.resolve_mut(location.registration)?;
            let removal = log.swap_remove(location.index);
            assert_eq!(removal.removed, base, "young block backlink mismatch");
            let pending_removal = if log.blocks().is_empty() {
                let pending_index = log.pending_index().expect("nonempty log was pending");
                let moved = log.remove_pending(&mut self.pending_mutators);
                Some((pending_index, moved))
            } else {
                None
            };
            (removal, pending_removal)
        };
        self.block_mut(slot).young_log_slot = None;
        if let Some(moved) = removal.moved {
            self.block_for_base_mut(moved).young_log_slot = Some(location);
        }
        if let Some((index, Some(moved))) = pending_removal {
            // The moved ID was already in this bound work index. An absent
            // canonical owner is internal corruption after mutation: unwind
            // through ConsStopAll so its existing poison policy fails closed.
            logs.resolve_mut(moved)
                .expect("pending canonical owner survives until drained")
                .repair_pending_index(index);
        }
        Ok(())
    }

    /// O(young blocks + pending owners), preserving every owner Vec capacity.
    /// Processing the LAST pending ID avoids moved-owner resolver/repair work.
    fn take_pending_young(
        &mut self,
        logs: &mut StoppedYoungLogs<'_>,
    ) -> Result<Vec<ConsBlockBase>, YoungRegistryError> {
        self.ensure_young_registry(logs)?;
        let mut bases = Vec::new();
        while let Some(&registration) = self.pending_mutators.last() {
            let first = bases.len();
            {
                // Domain refusal was handled before mutation. IDs are issued
                // only by the bound canonical registry, which retains retired
                // entries. Corruption here must poison the stopped capability,
                // not return a partial drain as a recoverable capture error.
                let log = logs
                    .resolve_mut(registration)
                    .expect("pending canonical owner survives until drained");
                assert_eq!(
                    log.pending_index().expect("queued log is pending").index(),
                    self.pending_mutators.len() - 1
                );
                assert!(!log.blocks().is_empty(), "queued log is nonempty");
                bases.extend(log.drain_blocks());
                assert!(
                    log.remove_pending(&mut self.pending_mutators).is_none(),
                    "last pending ID cannot move another owner"
                );
            }
            for &base in &bases[first..] {
                let block = self.block_for_base_mut(base);
                assert_eq!(
                    block
                        .young_log_slot
                        .expect("recorded young block")
                        .registration,
                    registration
                );
                block.young_log_slot = None;
            }
        }
        Ok(bases)
    }

    fn detach(&mut self, block_slot: BlockSlot) {
        let Some(available_slot) = self.block_mut(block_slot).available_slot.take() else {
            return;
        };
        let removed = self.blocks_with_free.swap_remove(available_slot);
        debug_assert_eq!(removed, block_slot);
        if let Some(moved) = self.blocks_with_free.get(available_slot).copied() {
            self.block_mut(moved).available_slot = Some(available_slot);
        }
    }

    fn attach_if_ready(&mut self, block_slot: BlockSlot) {
        let available_slot = self.blocks_with_free.len();
        let block = self.block_mut(block_slot);
        if block.count != 0
            && block.availability == BlockAvailability::Ready
            && block.available_slot.is_none()
        {
            block.available_slot = Some(available_slot);
            self.blocks_with_free.push(block_slot);
        }
    }
}

impl AllocatorState {
    pub(super) fn register(&mut self, block: &ConsBlock) {
        let state = self;
        let base = ConsBlockBase::of(block);
        assert!(
            !state.blocks.contains_key(&base),
            "cons block registered twice"
        );
        let metadata = BlockFreeState {
            base,
            young: AtomicBool::new(false),
            young_log_slot: None,
            head: 0,
            count: 0,
            available_slot: None,
            availability: BlockAvailability::Ready,
        };
        let slot = if let Some(slot) = state.vacant_slots.pop() {
            assert!(state.slots[slot].replace(metadata).is_none());
            slot
        } else {
            let slot = BlockSlot::for_append(&state.slots);
            state.slots.push(Some(metadata));
            slot
        };
        let previous = state.blocks.insert(base, slot);
        assert!(previous.is_none(), "cons block registered twice");
    }

    /// GEN0/C1 unrecorded-block path. A recorded GEN1 block must instead use
    /// ConsStopAll::unregister with the canonical stopped resolver.
    pub(super) fn unregister(&mut self, base: ConsBlockBase) {
        let Some(slot) = self.blocks.get(&base).copied() else {
            return;
        };
        let block = self.block(slot);
        assert!(
            block.young_log_slot.is_none(),
            "recorded block needs stopped unregister"
        );
        assert!(
            !block.young.load(Ordering::Relaxed),
            "young block needs stopped unregister"
        );
        self.remove_block_metadata(base, slot);
    }

    /// All canonical obligations are unlinked while the base still resolves.
    fn unregister_stopped(
        &mut self,
        base: ConsBlockBase,
        logs: &mut StoppedYoungLogs<'_>,
    ) -> Result<(), YoungRegistryError> {
        self.ensure_young_registry(logs)?;
        let Some(slot) = self.blocks.get(&base).copied() else {
            return Ok(());
        };
        assert!(
            self.mark_work.is_empty(),
            "mark obligations must finish before unregister"
        );
        self.unlink_young_log(base, logs)?;
        self.remove_block_metadata(base, slot);
        Ok(())
    }

    fn remove_block_metadata(&mut self, base: ConsBlockBase, slot: BlockSlot) {
        assert!(
            self.mark_work.is_empty(),
            "mark obligations must finish before unregister"
        );
        assert!(
            self.block(slot).young_log_slot.is_none(),
            "young backlink not unlinked"
        );
        self.detach(slot);
        self.blocks.remove(&base);
        let removed = self.slots[slot].take().expect("registered metadata slot");
        debug_assert_eq!(removed.base, base);
        // No slot escapes: canonical log/backlinks, base map and available
        // stack references are gone before this identity becomes reusable.
        self.vacant_slots.push(slot);
    }

    /// Record a region grant before its mutator can initialize any cell.
    /// The sole owner uses a unique plain borrow; admitted mutators hold the
    /// allocator mutex and deduplicate with a relaxed atomic swap. Only the
    /// false-to-true transition appends to the supplied canonical owner log.
    /// A wrong-heap log is rejected before any flag or log mutation.
    #[inline]
    pub(super) fn note_carve(
        &mut self,
        base: ConsBlockBase,
        mode: AllocatorAccessMode,
        log: &mut YoungBlockLog,
    ) -> Result<(), YoungRegistryError> {
        // Refuse a crossed heap binding BEFORE touching flag/backlink/log state.
        // Root supplies this existing-domain getter in the registry companion.
        if log.id().heap_identity() != self.identity {
            return Err(YoungRegistryError::WrongHeap);
        }
        #[cfg(test)]
        if test_fault_is(ConsAllocatorFault::SkipCarveRecording) {
            return Ok(());
        }
        let state = self;
        let slot = state.slot(base);
        let block = state.block_mut(slot);
        assert_eq!(
            block.availability,
            BlockAvailability::Ready,
            "carve from withheld block"
        );
        let newly_young = match mode {
            AllocatorAccessMode::Sole => {
                let young = block.young.get_mut();
                let newly_young = !*young;
                *young = true;
                newly_young
            }
            AllocatorAccessMode::Admitted => !block.young.swap(true, Ordering::Relaxed),
        };
        if newly_young {
            let index = log.push(base);
            log.ensure_pending(&mut state.pending_mutators);
            state.block_mut(slot).young_log_slot = Some(YoungLogSlot {
                registration: log.id(),
                index,
            });
        }
        #[cfg(test)]
        if test_fault_is(ConsAllocatorFault::CorruptAvailable) {
            // Only metadata is perturbed; no canonical payload is shared/read.
            let slot = state.slot(base);
            state.blocks_with_free.push(slot);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn mark_work_snapshot(&self) -> Vec<ConsBlockBase> {
        self.mark_work.clone()
    }

    /// # Safety
    /// Every free node must name live registered block storage, exclusively
    /// owned by this allocator until returned as a disjoint allocation run.
    /// Registration/storage lifetime must cover this call and the returned run.
    #[inline(always)]
    pub(super) unsafe fn reserve_run(&mut self, want: usize) -> Option<(*mut ConsCell, usize)> {
        debug_assert_ne!(want, 0);
        let state = self;
        let slot = *state.blocks_with_free.last()?;
        let block = state.block_mut(slot);
        let base = block.base;
        debug_assert_eq!(block.availability, BlockAvailability::Ready);
        let first = block.head as *mut ConsCell;
        debug_assert!(!first.is_null());
        let mut count = 1;
        // SAFETY: caller guarantees live exclusive free nodes; the sole-owner
        // borrow or shared mutex serializes list access until handout.
        let mut next = unsafe { (*first).free_next() };
        while count < want && next as usize == first as usize + count * size_of::<ConsCell>() {
            debug_assert_eq!(ConsBlockBase::of_cell(next), base);
            count += 1;
            // SAFETY: the next adjacent cell is a node of this locked list.
            next = unsafe { (*next).free_next() };
        }
        block.head = next as usize;
        block.count -= count;
        debug_assert_eq!(block.count == 0, next.is_null());
        if next.is_null() {
            state.detach(slot);
        }
        Some((first, count))
    }

    /// # Safety
    /// The range consists of unhanded cells of one still-live registered block,
    /// owned exclusively by the caller and absent from every free list. Its
    /// marks/ages must already have been cleared. No collector may concurrently
    /// sweep this range. A withheld block stays unavailable until publication.
    #[inline]
    pub(super) unsafe fn return_unused(&mut self, first: *mut ConsCell, unused: usize) {
        if unused == 0 {
            return;
        }
        let base = ConsBlockBase::of_cell(first);
        let state = self;
        let slot = state.slot(base);
        let block = state.block_mut(slot);
        for index in (0..unused).rev() {
            // SAFETY: the caller owns the entire unused range exclusively.
            let cell = unsafe { first.add(index) };
            debug_assert_eq!(ConsBlockBase::of_cell(cell), base);
            // SAFETY: linking exclusive unused storage to this block's list is
            // serialized with every reservation and list publication.
            unsafe { (*cell).set_free_next(block.head as *mut ConsCell) };
            block.head = cell as usize;
        }
        block.count += unused;
        state.attach_if_ready(slot);
    }

    /// # Safety
    /// `head` links exactly the `next_index - live` dead cells of `block`, with
    /// no live/reserved cell included, and the block remains alive until removed.
    /// Rebuilding has exclusive access to those cells; publication is last.
    pub(super) unsafe fn publish(&mut self, block: &ConsBlock, head: *mut ConsCell, live: usize) {
        let base = ConsBlockBase::of(block);
        let state = self;
        let slot = state.slot(base);
        state.detach(slot);
        let entry = state.block_mut(slot);
        entry.head = head as usize;
        entry.count = block.next_index as usize - live;
        debug_assert_eq!(head.is_null(), entry.count == 0);
        entry.availability = BlockAvailability::Ready;
        state.attach_if_ready(slot);
    }

    #[inline]
    pub(super) fn is_ready(&self, base: ConsBlockBase) -> bool {
        let state = self;
        state
            .blocks
            .get(&base)
            .is_some_and(|&slot| state.block(slot).availability == BlockAvailability::Ready)
    }

    #[cfg(test)]
    pub(super) fn free_count(&self) -> usize {
        self.slots.iter().flatten().map(|b| b.count).sum()
    }

    /// Copy counts under exclusive metadata access; telemetry must not walk free
    /// links after unlocking while another mutator reserves those cells.
    pub(super) fn free_counts(&self) -> Vec<(ConsBlockBase, usize)> {
        let state = self;
        state
            .blocks
            .iter()
            .map(|(base, &slot)| (*base, state.block(slot).count))
            .collect()
    }

    #[cfg(test)]
    pub(super) fn free_heads(&self) -> Vec<*mut ConsCell> {
        self.slots
            .iter()
            .flatten()
            .filter(|b| b.head != 0)
            .map(|b| b.head as *mut ConsCell)
            .collect()
    }

    #[cfg(test)]
    pub(super) fn verify(&self) {
        let state = self;
        let mut visited: TiVec<BlockSlot, bool> = vec![false; state.slots.len()].into();
        for &slot in &state.vacant_slots {
            assert!(!visited[slot], "duplicate vacant slot");
            visited[slot] = true;
            assert!(state.slots[slot].is_none(), "vacant slot is live");
        }
        for (available_slot, &slot) in state.blocks_with_free.iter().enumerate() {
            let block = state.block(slot);
            assert_eq!(block.available_slot, Some(available_slot));
            assert_eq!(block.availability, BlockAvailability::Ready);
            assert_ne!(block.count, 0);
        }
        for (base, &slot) in &state.blocks {
            assert!(!visited[slot], "slot reused by two block identities");
            visited[slot] = true;
            let block = state.block(slot);
            assert_eq!(block.base, *base);
            assert_eq!(
                block.available_slot.is_some(),
                block.availability == BlockAvailability::Ready && block.count != 0
            );
            let mut cursor = block.head as *mut ConsCell;
            let mut count = 0;
            while !cursor.is_null() {
                assert_eq!(ConsBlockBase::of_cell(cursor), *base);
                count += 1;
                assert!(count <= block.count, "duplicate/cyclic free node");
                // SAFETY: tests retain all registered blocks; mutex excludes
                // list writers while this verification follows free links.
                cursor = unsafe { (*cursor).free_next() };
            }
            assert_eq!(count, block.count);
        }
        assert!(visited.iter().all(|&seen| seen), "untracked metadata slot");
    }
}
/// Sticky admission storage: the discriminant replaces the World's former bool.
#[derive(Debug)]
enum AllocatorStorage {
    Sole(AllocatorState),
    Admitted(Mutex<AllocatorState>),
    Poisoned,
}
#[derive(Clone, Copy, Debug)]
enum AllocatorAccessMode {
    Sole,
    Admitted,
}

/// Confined owner. Only its mutex-only facade can cross threads.
#[derive(Debug)]
pub(super) struct ConsAllocator {
    storage: AllocatorStorage,
    _confined: std::marker::PhantomData<std::cell::Cell<()>>,
}
impl ConsAllocator {
    pub(super) fn new(identity: HeapIdentity) -> Self {
        Self {
            storage: AllocatorStorage::Sole(AllocatorState::new(identity)),
            _confined: std::marker::PhantomData,
        }
    }
}
static_assertions::assert_impl_all!(ConsAllocator: Send);
static_assertions::assert_not_impl_any!(ConsAllocator: Sync);

/// Borrowing this view excludes exclusive owner access. It carries no mode
/// cell or raw owner pointer and never exposes unlocked metadata.
#[derive(Clone, Copy, Debug)]
pub(super) struct SharedConsAllocator<'a> {
    state: &'a Mutex<AllocatorState>,
}
static_assertions::assert_impl_all!(SharedConsAllocator<'_>: Send, Sync);

impl ConsAllocator {
    #[inline]
    pub(super) fn threads_admitted(&self) -> bool {
        matches!(self.storage, AllocatorStorage::Admitted(_))
    }
    #[cfg_attr(not(test), allow(dead_code))] // Future production mutators use this explicit facade.
    pub(super) fn shared(&self) -> SharedConsAllocator<'_> {
        let AllocatorStorage::Admitted(state) = &self.storage else {
            panic!("allocator sharing requires stopped-world admission");
        };
        SharedConsAllocator { state }
    }
    fn with_state<R>(
        &self,
        operation: impl FnOnce(&AllocatorState, AllocatorAccessMode) -> R,
    ) -> R {
        match &self.storage {
            AllocatorStorage::Sole(state) => operation(state, AllocatorAccessMode::Sole),
            AllocatorStorage::Admitted(mutex) => operation(
                &mutex.lock().expect("cons allocator mutex poisoned"),
                AllocatorAccessMode::Admitted,
            ),
            AllocatorStorage::Poisoned => panic!("cons allocator stopped-world state poisoned"),
        }
    }
    fn with_state_mut<R>(
        &mut self,
        operation: impl FnOnce(&mut AllocatorState, AllocatorAccessMode) -> R,
    ) -> R {
        match &mut self.storage {
            // No callback enters these algorithms. On a valid state, region
            // reserve/return cannot unwind after a partial commit: their
            // storage/range contract and private list invariants establish
            // all indexing and arithmetic before handout. Allocation failure
            // aborts. GC-driver unwinds are independently fail-closed through
            // gc_driver_active; stop-all metadata guards poison on unwind.
            // Thus no per-refill global panic check or atomic RMW is needed.
            AllocatorStorage::Sole(state) => operation(state, AllocatorAccessMode::Sole),
            AllocatorStorage::Admitted(mutex) => operation(
                &mut mutex.lock().expect("cons allocator mutex poisoned"),
                AllocatorAccessMode::Admitted,
            ),
            AllocatorStorage::Poisoned => panic!("cons allocator stopped-world state poisoned"),
        }
    }
    pub(super) fn is_poisoned(&self) -> bool {
        match &self.storage {
            AllocatorStorage::Sole(_) => false,
            AllocatorStorage::Admitted(mutex) => mutex.is_poisoned(),
            AllocatorStorage::Poisoned => true,
        }
    }
    /// # Safety
    /// Park all mutators, close regions, join the marker, retain every block,
    /// and exclude callbacks/nested collection until this guard drops.
    pub(super) unsafe fn stopped_world(&mut self) -> ConsStopAll<'_> {
        let state = match &mut self.storage {
            storage @ AllocatorStorage::Sole(_) => StoppedAllocatorState::Owner {
                storage,
                panicking_on_entry: std::thread::panicking(),
            },
            AllocatorStorage::Admitted(mutex) => StoppedAllocatorState::Admitted(
                mutex.lock().expect("cons allocator mutex poisoned"),
            ),
            AllocatorStorage::Poisoned => panic!("cons allocator stopped-world state poisoned"),
        };
        ConsStopAll {
            state,
            _owner: std::marker::PhantomData,
        }
    }

    pub(super) fn register(&mut self, block: &ConsBlock) {
        self.with_state_mut(|state, _mode| state.register(block))
    }

    pub(super) fn unregister(&mut self, base: ConsBlockBase) {
        self.with_state_mut(|state, _mode| state.unregister(base))
    }

    /// Record a region grant before its mutator can initialize any cell.
    /// The sole owner uses a unique plain borrow; admitted mutators hold the
    /// allocator mutex and deduplicate with a relaxed atomic swap. Only the
    /// false-to-true transition appends to the supplied canonical owner log.
    /// A wrong-heap log is rejected before any flag or log mutation.
    #[inline]
    pub(super) fn note_carve(
        &mut self,
        base: ConsBlockBase,
        log: &mut YoungBlockLog,
    ) -> Result<(), YoungRegistryError> {
        self.with_state_mut(|state, mode| state.note_carve(base, mode, log))
    }

    #[cfg(test)]
    pub(super) fn mark_work_snapshot(&self) -> Vec<ConsBlockBase> {
        self.with_state(|state, _mode| state.mark_work_snapshot())
    }

    /// # Safety
    /// Every free node must name live registered block storage, exclusively
    /// owned by this allocator until returned as a disjoint allocation run.
    /// Registration/storage lifetime must cover this call and the returned run.
    // Load-bearing inlining boundary: keep the sole run algorithm in its
    // refill caller, without a per-refill FnOnce closure ABI/prologue. The
    // admitted guard protocol remains in its separate helper below.
    #[inline]
    pub(super) unsafe fn reserve_run(&mut self, want: usize) -> Option<(*mut ConsCell, usize)> {
        match &mut self.storage {
            AllocatorStorage::Sole(state) => {
                // SAFETY: this exclusive owner loan retains the caller's
                // registered-storage and disjoint-node contract above.
                unsafe { state.reserve_run(want) }
            }
            AllocatorStorage::Admitted(mutex) => {
                // SAFETY: the caller retains every registered block; the
                // helper serializes list access through the existing mutex.
                unsafe { Self::reserve_run_admitted(mutex, want) }
            }
            AllocatorStorage::Poisoned => panic!("cons allocator stopped-world state poisoned"),
        }
    }

    // Admission is cold relative to the default sole-owner configuration.
    // Keep its guard protocol out of the sole handout's inlining decision.
    // This changes no serialization or storage-lifetime contract.
    #[cold]
    #[inline(never)]
    unsafe fn reserve_run_admitted(
        mutex: &Mutex<AllocatorState>,
        want: usize,
    ) -> Option<(*mut ConsCell, usize)> {
        let mut state = mutex.lock().expect("cons allocator mutex poisoned");
        // SAFETY: reserve_run's caller retains the registered block storage
        // and free-node contract, and this guard owns the metadata loan.
        unsafe { state.reserve_run(want) }
    }

    /// # Safety
    /// The range consists of unhanded cells of one still-live registered block,
    /// owned exclusively by the caller and absent from every free list. Its
    /// marks/ages must already have been cleared. No collector may concurrently
    /// sweep this range. A withheld block stays unavailable until publication.
    #[inline]
    pub(super) unsafe fn return_unused(&mut self, first: *mut ConsCell, unused: usize) {
        self.with_state_mut(|state, _mode| {
            // SAFETY: this metadata loan is exclusive; the caller retains
            // the registered storage and node-ownership contract above.
            unsafe { state.return_unused(first, unused) }
        })
    }

    /// # Safety
    /// `head` links exactly the `next_index - live` dead cells of `block`, with
    /// no live/reserved cell included, and the block remains alive until removed.
    /// Rebuilding has exclusive access to those cells; publication is last.
    pub(super) unsafe fn publish(&mut self, block: &ConsBlock, head: *mut ConsCell, live: usize) {
        self.with_state_mut(|state, _mode| {
            // SAFETY: this metadata loan is exclusive; the caller retains
            // the registered storage and node-ownership contract above.
            unsafe { state.publish(block, head, live) }
        })
    }

    #[inline]
    pub(super) fn is_ready(&self, base: ConsBlockBase) -> bool {
        self.with_state(|state, _mode| state.is_ready(base))
    }

    #[cfg(test)]
    pub(super) fn free_count(&self) -> usize {
        self.with_state(|state, _mode| state.free_count())
    }

    /// Copy counts under exclusive metadata access; telemetry must not walk free
    /// links after unlocking while another mutator reserves those cells.
    pub(super) fn free_counts(&self) -> Vec<(ConsBlockBase, usize)> {
        self.with_state(|state, _mode| state.free_counts())
    }

    #[cfg(test)]
    pub(super) fn free_heads(&self) -> Vec<*mut ConsCell> {
        self.with_state(|state, _mode| state.free_heads())
    }

    #[cfg(test)]
    pub(super) fn verify(&self) {
        self.with_state(|state, _mode| state.verify())
    }
}
#[allow(dead_code)] // Complete explicit N-mutator facade; production uses the confined owner.
// Retain stage-specific operations so shared callers never need the owner.
impl SharedConsAllocator<'_> {
    fn with_state<R>(
        &self,
        operation: impl FnOnce(&AllocatorState, AllocatorAccessMode) -> R,
    ) -> R {
        operation(
            &self.state.lock().expect("cons allocator mutex poisoned"),
            AllocatorAccessMode::Admitted,
        )
    }
    fn with_state_mut<R>(
        &self,
        operation: impl FnOnce(&mut AllocatorState, AllocatorAccessMode) -> R,
    ) -> R {
        operation(
            &mut self.state.lock().expect("cons allocator mutex poisoned"),
            AllocatorAccessMode::Admitted,
        )
    }
    pub(super) fn is_poisoned(&self) -> bool {
        self.state.is_poisoned()
    }
    /// # Safety
    /// Park every mutator with regions closed, join the marker, and retain all
    /// registered block storage until this guard drops.
    pub(super) unsafe fn stopped_world(&self) -> ConsStopAll<'_> {
        ConsStopAll {
            state: StoppedAllocatorState::Admitted(
                self.state.lock().expect("cons allocator mutex poisoned"),
            ),
            _owner: std::marker::PhantomData,
        }
    }

    pub(super) fn register(&self, block: &ConsBlock) {
        self.with_state_mut(|state, _mode| state.register(block))
    }

    pub(super) fn unregister(&self, base: ConsBlockBase) {
        self.with_state_mut(|state, _mode| state.unregister(base))
    }

    /// Record a region grant before its mutator can initialize any cell.
    /// All list/flag metadata shares the allocator mutex. The relaxed atomic
    /// swap deduplicates carves across mutators; only its false winner
    /// appends to this worker's uniquely owned canonical young-only loan.
    #[inline]
    pub(super) fn note_carve(
        &self,
        base: ConsBlockBase,
        log: &mut YoungBlockLog,
    ) -> Result<(), YoungRegistryError> {
        self.with_state_mut(|state, mode| state.note_carve(base, mode, log))
    }

    #[cfg(test)]
    pub(super) fn mark_work_snapshot(&self) -> Vec<ConsBlockBase> {
        self.with_state(|state, _mode| state.mark_work_snapshot())
    }

    /// # Safety
    /// Every free node must name live registered block storage, exclusively
    /// owned by this allocator until returned as a disjoint allocation run.
    /// Registration/storage lifetime must cover this call and the returned run.
    #[inline]
    pub(super) unsafe fn reserve_run(&self, want: usize) -> Option<(*mut ConsCell, usize)> {
        self.with_state_mut(|state, _mode| {
            // SAFETY: this metadata loan is exclusive; the caller retains
            // the registered storage and node-ownership contract above.
            unsafe { state.reserve_run(want) }
        })
    }

    /// # Safety
    /// The range consists of unhanded cells of one still-live registered block,
    /// owned exclusively by the caller and absent from every free list. Its
    /// marks/ages must already have been cleared. No collector may concurrently
    /// sweep this range. A withheld block stays unavailable until publication.
    #[inline]
    pub(super) unsafe fn return_unused(&self, first: *mut ConsCell, unused: usize) {
        self.with_state_mut(|state, _mode| {
            // SAFETY: this metadata loan is exclusive; the caller retains
            // the registered storage and node-ownership contract above.
            unsafe { state.return_unused(first, unused) }
        })
    }

    /// # Safety
    /// `head` links exactly the `next_index - live` dead cells of `block`, with
    /// no live/reserved cell included, and the block remains alive until removed.
    /// Rebuilding has exclusive access to those cells; publication is last.
    pub(super) unsafe fn publish(&self, block: &ConsBlock, head: *mut ConsCell, live: usize) {
        self.with_state_mut(|state, _mode| {
            // SAFETY: this metadata loan is exclusive; the caller retains
            // the registered storage and node-ownership contract above.
            unsafe { state.publish(block, head, live) }
        })
    }

    #[inline]
    pub(super) fn is_ready(&self, base: ConsBlockBase) -> bool {
        self.with_state(|state, _mode| state.is_ready(base))
    }

    #[cfg(test)]
    pub(super) fn free_count(&self) -> usize {
        self.with_state(|state, _mode| state.free_count())
    }

    /// Copy counts under exclusive metadata access; telemetry must not walk free
    /// links after unlocking while another mutator reserves those cells.
    pub(super) fn free_counts(&self) -> Vec<(ConsBlockBase, usize)> {
        self.with_state(|state, _mode| state.free_counts())
    }

    #[cfg(test)]
    pub(super) fn free_heads(&self) -> Vec<*mut ConsCell> {
        self.with_state(|state, _mode| state.free_heads())
    }

    #[cfg(test)]
    pub(super) fn verify(&self) {
        self.with_state(|state, _mode| state.verify())
    }
}

/// Capability for stopped-world metadata changes. Safe callers cannot clear
/// flags without constructing this guard at the unsafe stop-all boundary.
#[derive(Debug)]
#[must_use = "keep the stopped-world guard alive for collector metadata changes"]
pub(super) struct ConsStopAll<'a> {
    state: StoppedAllocatorState<'a>,
    _owner: std::marker::PhantomData<std::rc::Rc<()>>,
}

static_assertions::assert_not_impl_any!(ConsStopAll<'_>: Send, Sync);

/// Panic bookkeeping is confined to collector stop-all extents, never refill.
#[derive(Debug)]
enum StoppedAllocatorState<'a> {
    Owner {
        storage: &'a mut AllocatorStorage,
        panicking_on_entry: bool,
    },
    Admitted(MutexGuard<'a, AllocatorState>),
}
impl std::ops::Deref for StoppedAllocatorState<'_> {
    type Target = AllocatorState;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owner { storage, .. } => match &**storage {
                AllocatorStorage::Sole(state) => state,
                AllocatorStorage::Admitted(_) | AllocatorStorage::Poisoned => {
                    panic!("owner stop-all capability consumed by admission")
                }
            },
            Self::Admitted(state) => state,
        }
    }
}
impl std::ops::DerefMut for StoppedAllocatorState<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Owner { storage, .. } => match &mut **storage {
                AllocatorStorage::Sole(state) => state,
                AllocatorStorage::Admitted(_) | AllocatorStorage::Poisoned => {
                    panic!("owner stop-all capability consumed by admission")
                }
            },
            Self::Admitted(state) => state,
        }
    }
}
impl Drop for StoppedAllocatorState<'_> {
    fn drop(&mut self) {
        if let Self::Owner {
            storage,
            panicking_on_entry,
        } = self
        {
            if !*panicking_on_entry && std::thread::panicking() {
                **storage = AllocatorStorage::Poisoned;
            }
        }
    }
}

impl ConsStopAll<'_> {
    /// One-way admission commits at the stop-all boundary. Consuming this
    /// capability prevents using its former unlocked state after publication.
    pub(super) fn admit_threads(mut self) {
        let StoppedAllocatorState::Owner { storage, .. } = &mut self.state else {
            panic!("admission requires the sole-owner stop-all capability");
        };
        let AllocatorStorage::Sole(state) =
            std::mem::replace(&mut **storage, AllocatorStorage::Poisoned)
        else {
            panic!("thread admission is one-way");
        };
        **storage = AllocatorStorage::Admitted(Mutex::new(state));
    }

    /// Snapshot the current minor's mark work; flags remain SET until promotion.
    /// The owned return lets the heap drop this lock before marking blocks.
    pub(super) fn capture_minor(
        &mut self,
        logs: &mut StoppedYoungLogs<'_>,
    ) -> Result<Vec<ConsBlockBase>, YoungRegistryError> {
        self.state.ensure_young_registry(logs)?;
        assert!(
            self.state.mark_work.is_empty(),
            "previous mark work not promoted"
        );
        self.state.mark_work = self.state.take_pending_young(logs)?;
        Ok(self.state.mark_work.clone())
    }

    /// O(1) canonical unlink precedes base-map removal/address reuse. This is
    /// the GEN1 release path, including carved-but-entirely-refunded candidates.
    pub(super) fn unregister(
        &mut self,
        base: ConsBlockBase,
        logs: &mut StoppedYoungLogs<'_>,
    ) -> Result<(), YoungRegistryError> {
        self.state.unregister_stopped(base, logs)
    }

    /// This is the ONLY young-flag clear site. Called immediately after all
    /// promotion, still stopped. Stage 2 records flags without selecting sweep
    /// work, so existing full-sweep withholding remains separate. Stage 3 will
    /// move mark_work into sweep_work here and withhold its lists atomically.
    ///
    /// Pending logs also cover a major, whose young set includes allocations
    /// performed while its worker marked. Flags clear at major TERMINATION,
    /// never at sweep completion, preserving later sweep-window carves.
    pub(super) fn finish_promotion(
        &mut self,
        logs: &mut StoppedYoungLogs<'_>,
    ) -> Result<(), YoungRegistryError> {
        self.state.ensure_young_registry(logs)?;
        // Preserve stage2: flags are tracked/cleared but do not select mark or
        // sweep work. Full-sweep withholding remains its existing separate call.
        let pending = self.state.take_pending_young(logs)?;
        let mut promoted = std::mem::take(&mut self.state.mark_work);
        promoted.extend(pending);
        for base in promoted {
            let block = self.state.block_for_base_mut(base);
            debug_assert!(block.young.load(Ordering::Relaxed));
            #[cfg(test)]
            if test_fault_is(ConsAllocatorFault::KeepFlagAfterPromotion) {
                continue;
            }
            block.young.store(false, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Existing full sweep entry uses this in stage 2; O(worklist length).
    pub(super) fn withhold(&mut self, bases: impl IntoIterator<Item = ConsBlockBase>) {
        for base in bases {
            let slot = self.state.slot(base);
            self.state.detach(slot);
            let block = self.state.block_mut(slot);
            block.head = 0;
            block.count = 0;
            block.availability = BlockAvailability::WithheldForSweep;
        }
    }

    fn verify_registry(&self, logs: &StoppedYoungLogs<'_>) -> Result<(), YoungRegistryError> {
        self.state.ensure_young_registry(logs)?;
        self.state.verify_slots();
        let mut listed = std::collections::HashSet::new();
        let mut pending = std::collections::HashSet::new();
        for registration in &self.state.pending_mutators {
            assert_eq!(registration.heap_identity(), self.state.identity);
            assert!(
                pending.insert(*registration),
                "duplicate pending canonical owner"
            );
        }
        // Deliberate verify-only enumeration of the SAME SATB cold registry,
        // including retired owners; ordinary capture visits only pending IDs.
        for log in logs.logs_for_verify() {
            assert_eq!(log.id().heap_identity(), self.state.identity);
            assert_eq!(log.pending_index().is_some(), !log.blocks().is_empty());
            if let Some(index) = log.pending_index() {
                assert_eq!(self.state.pending_mutators[index.index()], log.id());
                assert!(
                    pending.remove(&log.id()),
                    "canonical log missing pending ID"
                );
            }
            for (index, base) in log.blocks().iter().enumerate() {
                assert!(listed.insert(*base), "duplicate young log entry");
                let block = self.state.block_for_base(*base);
                let backlink = block
                    .young_log_slot
                    .expect("young block has canonical backlink");
                assert_eq!(backlink.registration, log.id());
                assert_eq!(backlink.index.index(), index);
            }
        }
        assert!(pending.is_empty(), "pending ID has no canonical owner log");
        for base in &self.state.mark_work {
            assert!(listed.insert(*base), "duplicate captured young work");
            assert!(self.state.block_for_base(*base).young_log_slot.is_none());
        }
        for (base, &slot) in &self.state.blocks {
            let block = self.state.block(slot);
            assert_eq!(
                block.young.load(Ordering::Relaxed),
                listed.contains(base),
                "young flag/log mismatch at {base:?}"
            );
        }
        // Check availability here as well, so CorruptAvailable is caught by
        // the actual GEN1 verifier, not just by a cfg(test) list helper.
        let mut available = std::collections::HashSet::new();
        for (available_slot, &slot) in self.state.blocks_with_free.iter().enumerate() {
            let block = self.state.block(slot);
            assert!(available.insert(block.base), "duplicate available block");
            assert_eq!(block.available_slot, Some(available_slot));
            assert_eq!(block.availability, BlockAvailability::Ready);
            assert_ne!(block.count, 0);
        }
        for (base, &slot) in &self.state.blocks {
            let block = self.state.block(slot);
            assert_eq!(
                block.available_slot.is_some(),
                block.availability == BlockAvailability::Ready && block.count != 0
            );
            assert_eq!(block.available_slot.is_some(), available.contains(base));
        }
        Ok(())
    }

    /// At minor BEGIN the previous sweep is finished, all regions are closed,
    /// and every physically initialized non-old cell must be in the young set.
    pub(super) fn verify_young_at_begin(
        &self,
        blocks: &[ConsBlock],
        logs: &StoppedYoungLogs<'_>,
    ) -> Result<(), YoungRegistryError> {
        self.verify_registry(logs)?;
        self.verify_stale_marks(blocks);
        for block in blocks {
            let metadata = self.state.block_for_base(ConsBlockBase::of(block));
            for index in 0..block.next_index as usize {
                // SAFETY: this guard proves stopped mutators/closed regions,
                // and its caller retains every listed block's storage.
                let cell = unsafe { &*block.cells_ptr().add(index) };
                if cell.car.bits() != TaggedValue::DEAD.bits() && !block.trailer().is_old(index) {
                    assert!(
                        metadata.young.load(Ordering::Relaxed),
                        "allocated young cell missing flag"
                    );
                }
            }
        }
        Ok(())
    }

    /// After promotion unmarked dying young cells are still initialized until
    /// sweep. Do not apply begin's physical-cell coverage check at this stage.
    pub(super) fn verify_after_promotion(
        &self,
        blocks: &[ConsBlock],
        logs: &StoppedYoungLogs<'_>,
    ) -> Result<(), YoungRegistryError> {
        self.verify_registry(logs)?;
        self.verify_stale_marks(blocks);
        Ok(())
    }

    fn verify_stale_marks(&self, blocks: &[ConsBlock]) {
        for block in blocks {
            let metadata = self.state.block_for_base(ConsBlockBase::of(block));
            if !metadata.young.load(Ordering::Relaxed) {
                for index in 0..block.next_index as usize {
                    assert!(
                        !block.trailer().is_marked(index) || block.trailer().is_old(index),
                        "non-young block has marked non-old cell"
                    );
                }
            }
        }
    }
}

/// Mutation switches affect ONLY the current test thread. They carry no Lisp
/// runtime state and are restored on unwind by the non-Send RAII guard.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConsAllocatorFault {
    SkipCarveRecording,
    KeepFlagAfterPromotion,
    CorruptAvailable,
}

#[cfg(test)]
thread_local! {
    static CONS_ALLOCATOR_FAULT: std::cell::Cell<Option<ConsAllocatorFault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn test_fault_is(fault: ConsAllocatorFault) -> bool {
    CONS_ALLOCATOR_FAULT.with(|slot| slot.get() == Some(fault))
}

#[cfg(test)]
#[derive(Debug)]
#[must_use = "hold fault injection for the test extent"]
pub(super) struct ConsAllocatorFaultGuard {
    previous: Option<ConsAllocatorFault>,
    _owner: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
static_assertions::assert_not_impl_any!(ConsAllocatorFaultGuard: Send, Sync);

#[cfg(test)]
impl ConsAllocatorFaultGuard {
    pub(super) fn inject(fault: ConsAllocatorFault) -> Self {
        let previous = CONS_ALLOCATOR_FAULT.with(|slot| slot.replace(Some(fault)));
        Self {
            previous,
            _owner: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
impl Drop for ConsAllocatorFaultGuard {
    fn drop(&mut self) {
        CONS_ALLOCATOR_FAULT.with(|slot| slot.set(self.previous));
    }
}

impl TaggedHeap {
    /// The World's one-time thread admission state, owned alongside allocator
    /// metadata so reservation and thread admission cannot disagree.
    #[inline]
    pub(crate) fn threads_admitted(&self) -> bool {
        self.cons_allocator.threads_admitted()
    }

    /// Commit thread admission while the sole owner is stopped.
    ///
    /// # Safety
    /// The caller owns the stopped sole Context, excludes concurrent-marker
    /// launches and callbacks, and has drained the collector to quiescence.
    /// No second mutator may run until this transition returns.
    pub(crate) unsafe fn admit_threads_world_stopped(&mut self) {
        debug_assert!(!self.mark_in_progress);
        debug_assert!(!self.generational.major_in_progress);
        self.close_alloc_regions();
        // SAFETY: the caller retains the stopped sole-owner/closed-collector
        // extent, and closing above returns every unhanded region tail.
        unsafe { self.cons_allocator.stopped_world() }.admit_threads();
    }
}

#[cfg(test)]
#[path = "tests/cons_allocator_test.rs"]
mod tests;
