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

use super::*;
use std::num::NonZeroUsize;
use std::sync::{Mutex, MutexGuard};
use typed_index_collections::TiVec;

/// Own the cold allocator without introducing a pointer niche into the inline
/// heap. A bare Box changes repr(Rust) field ordering and JIT offsets; the
/// Option's null representation preserves the former raw-pointer layout.
/// Construction always installs Some, and the field is private thereafter.
#[derive(Debug)]
#[repr(transparent)]
pub(super) struct ConsAllocatorOwner(Option<Box<ConsAllocator>>);

impl Default for ConsAllocatorOwner {
    fn default() -> Self {
        Self(Some(Box::default()))
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
    head: usize,
    count: usize,
    available_slot: Option<usize>,
    availability: BlockAvailability,
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

#[derive(Default)]
struct AllocatorState {
    blocks: FxHashMap<ConsBlockBase, BlockSlot>,
    slots: TiVec<BlockSlot, Option<BlockFreeState>>,
    vacant_slots: Vec<BlockSlot>,
    blocks_with_free: Vec<BlockSlot>,
}

impl std::fmt::Debug for AllocatorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // TiVec's Debug implementation enumerates typed keys and requires an
        // inbound usize conversion. Keep slot construction private instead.
        f.debug_struct("AllocatorState")
            .field("blocks", &self.blocks)
            .field("slots", &self.slots.raw)
            .field("vacant_slots", &self.vacant_slots)
            .field("blocks_with_free", &self.blocks_with_free)
            .finish()
    }
}

impl AllocatorState {
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

    pub(super) fn unregister(&mut self, base: ConsBlockBase) {
        let state = self;
        let Some(slot) = state.blocks.remove(&base) else {
            return;
        };
        state.detach(slot);
        let removed = state.slots[slot].take().expect("registered metadata slot");
        debug_assert_eq!(removed.base, base);
        // No slot escapes this state: map and available-stack references have
        // both gone before the identity becomes reusable. Storage stays owned
        // by ConsBlock and is released only after this method returns.
        state.vacant_slots.push(slot);
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
impl Default for ConsAllocator {
    fn default() -> Self {
        Self {
            storage: AllocatorStorage::Sole(AllocatorState::default()),
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

/// Collector capability; neither moving it to another thread nor constructing
/// it without the unsafe parked-world proof is possible through the safe API.
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

    /// O(worklist length), including indexed available-stack removal.
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
