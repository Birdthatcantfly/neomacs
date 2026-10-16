//! GC state written only by the mutator.
//!
//! The current runtime has one mutator. Keeping its allocation accounting and
//! barrier state in one inline value gives collector consumers the same
//! accessor surface a future mutator registry will expose, without adding a
//! pointer indirection to allocation or a new thread-local cache.

use super::*;

pub(super) struct MutatorGcState {
    /// Charged objects, including the unused tail of an open P0.8 region.
    pub(super) allocated_count: usize,
    pub(super) memory_use_counts: [u64; MEMORY_USE_COUNT_LEN],
    pub(super) bytes_since_gc: usize,
    /// Bytes charged before the most recent accounting reset.
    pub(super) bytes_banked_at_resets: u64,
    /// Ordinary old owners recorded since the previous collection began.
    pub(super) remset: Vec<TaggedValue>,
    /// Mapped owners whose children must be scanned in the next cycle.
    pub(super) r_mapped_seen: FxHashSet<usize>,
    /// Non-cons objects born black during a concurrent major (C2.6).
    pub(super) black_born: Vec<*mut GcHeader>,
    pub(super) black_born_regions: Vec<birth_logs::BlackBornRegion>,
    pub(super) black_cons_region_start: Option<usize>,
    pub(super) black_float_region_start: Option<usize>,
    /// Symbol side-table preimages and inserted symbols belong to this mutator.
    pub(super) major_symbol_preimages: Vec<SymId>,
    /// Cons owners written during a concurrent major. Their current children
    /// are traced once per owner after the stopped-world join; old insertions
    /// are not necessarily allocate-black births. No free precedes this drain.
    pub(super) major_cons_writes: Vec<TaggedValue>,
    /// Direct-mapped repeat-owner reject, cleared at every cycle begin.
    pub(super) remembered_cache: [usize; BARRIER_CACHE_SLOTS],
    /// Generation pacing obligations owned by this mutator, summed at poll.
    pub(super) pacing: pacing::GenerationPacingCounters,
}

/// Unused region capacity, derived from the cursors shared with compiled code.
/// Deriving this at a read keeps exact accounting out of the allocation path.
#[derive(Clone, Copy)]
pub(super) struct AllocationRegionDeltas {
    pub(super) conses: usize,
    pub(super) floats: usize,
}

impl AllocationRegionDeltas {
    #[inline]
    fn objects(self) -> usize {
        self.conses + self.floats
    }

    #[inline]
    fn bytes(self) -> usize {
        self.conses * size_of::<ConsCell>() + self.floats * size_of::<FloatObj>()
    }
}

impl MutatorGcState {
    pub(super) fn new() -> Self {
        Self {
            allocated_count: 0,
            memory_use_counts: [0; MEMORY_USE_COUNT_LEN],
            bytes_since_gc: 0,
            bytes_banked_at_resets: 0,
            remset: Vec::new(),
            r_mapped_seen: FxHashSet::default(),
            black_born: Vec::new(),
            black_born_regions: Vec::new(),
            black_cons_region_start: None,
            black_float_region_start: None,
            major_symbol_preimages: Vec::new(),
            major_cons_writes: Vec::new(),
            remembered_cache: [0; BARRIER_CACHE_SLOTS],
            pacing: pacing::GenerationPacingCounters::default(),
        }
    }

    #[inline]
    pub(super) fn exact_allocated_count(&self, delta: AllocationRegionDeltas) -> usize {
        self.allocated_count - delta.objects()
    }

    #[inline]
    pub(super) fn exact_bytes_since_gc(&self, delta: AllocationRegionDeltas) -> usize {
        self.bytes_since_gc - delta.bytes()
    }

    #[inline]
    pub(super) fn exact_memory_use_counts(
        &self,
        delta: AllocationRegionDeltas,
    ) -> [u64; MEMORY_USE_COUNT_LEN] {
        let mut counts = self.memory_use_counts;
        let conses = MemoryUseCountSlot::ConsCells.index();
        counts[conses] = counts[conses].wrapping_sub(delta.conses as u64);
        let floats = MemoryUseCountSlot::Floats.index();
        counts[floats] = counts[floats].wrapping_sub(delta.floats as u64);
        counts
    }
}

impl TaggedHeap {
    #[inline]
    pub(super) fn current_mutator_gc(&self) -> &MutatorGcState {
        &self.mutator_gc
    }

    #[inline]
    pub(super) fn current_mutator_gc_mut(&mut self) -> &mut MutatorGcState {
        &mut self.mutator_gc
    }

    #[inline]
    pub(super) fn mutators(&self) -> impl Iterator<Item = &MutatorGcState> {
        #[cfg(not(test))]
        {
            std::iter::once(self.current_mutator_gc())
        }
        #[cfg(test)]
        {
            std::iter::once(&self.mutator_gc).chain(
                self._satb_layout_reserve
                    .0
                    .registered
                    .iter()
                    .map(|entry| &entry.gc),
            )
        }
    }

    #[inline]
    pub(super) fn mutators_mut(&mut self) -> impl Iterator<Item = &mut MutatorGcState> {
        #[cfg(not(test))]
        {
            std::iter::once(self.current_mutator_gc_mut())
        }
        #[cfg(test)]
        {
            std::iter::once(&mut self.mutator_gc).chain(
                self._satb_layout_reserve
                    .0
                    .registered
                    .iter_mut()
                    .map(|entry| &mut entry.gc),
            )
        }
    }

    #[inline]
    pub(super) fn allocation_region_deltas(&self) -> AllocationRegionDeltas {
        AllocationRegionDeltas {
            conses: self.open_cons_unused(),
            floats: self.open_float_unused(),
        }
    }
}

// U35's cold per-mutator accessors share this accessor family. Hot allocation
// and barrier callers keep the original inline MutatorGcState above.
impl TaggedHeap {
    pub(super) fn current_concurrent_hash_mutator(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<cold_gc::ConcurrentHashMutatorState>> {
        self.concurrent_claims_state()
            .expect("claims enabled")
            .current_mutator()
    }

    /// The caller has stopped all registered mutators. Copy Arc handles under
    /// the map lock, then release it before locking any log/descriptor entry.
    pub(super) fn concurrent_hash_mutators(
        &self,
    ) -> impl Iterator<Item = std::sync::Arc<std::sync::Mutex<cold_gc::ConcurrentHashMutatorState>>>
    {
        self.concurrent_claims_state()
            .map(|state| state.mutators_world_stopped())
            .unwrap_or_default()
            .into_iter()
    }
}

use super::satb::{SatbBuffer, SatbHandoff, SatbPoisoned, SatbRetired};
use super::scan_contract::{SatbDrainError, StoppedMutatorsProof};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SatbMutatorLifecycle {
    #[default]
    Active,
    Retired,
}

/// Owner-confined cold half of the same logical MutatorGcState. The existing
/// inline hot half stays exactly 800 bytes; no inactive-mark path follows this
/// sidecar pointer. A retired half remains registered until stopped termination
/// has folded its partial and consumed its dirty-owner insertion repair log.
#[derive(Debug)]
pub(super) struct SatbMutatorState {
    pub(super) satb: SatbBuffer,
    pub(super) satb_snapshotted_owners: FxHashSet<usize>,
    pub(super) satb_string_preimage_addrs: FxHashSet<usize>,
    lifecycle: SatbMutatorLifecycle,
}

static_assertions::assert_not_impl_any!(SatbMutatorState: Send, Sync, Clone, Copy);
static_assertions::assert_impl_all!(SatbMutatorState: std::fmt::Debug);

impl SatbMutatorState {
    pub(super) fn new() -> Self {
        Self {
            satb: SatbBuffer::new(),
            satb_snapshotted_owners: FxHashSet::default(),
            satb_string_preimage_addrs: FxHashSet::default(),
            lifecycle: SatbMutatorLifecycle::Active,
        }
    }

    #[must_use = "retirement must succeed before removing the registration"]
    pub(super) fn retire(&mut self, handoff: &SatbHandoff) -> Result<SatbRetired, SatbPoisoned> {
        let retired = std::mem::take(&mut self.satb).retire(handoff);
        self.lifecycle = SatbMutatorLifecycle::Retired;
        retired
    }

    /// Residual words are an error, never a reason to clear the log at begin.
    pub(super) fn begin_cycle(&mut self, _: &StoppedMutatorsProof) -> Result<(), SatbDrainError> {
        if !self.satb.is_empty() {
            return Err(SatbDrainError::Residual);
        }
        self.satb_snapshotted_owners.clear();
        self.satb_string_preimage_addrs.clear();
        Ok(())
    }
}

/// One registered test owner includes a real hot MutatorGcState, not merely a
/// transport buffer. The ordinary iterator family and the stopped SATB fold
/// both enumerate this exact same set of registrations.
#[cfg(test)]
struct RegisteredMutatorGc {
    gc: MutatorGcState,
    satb: SatbMutatorState,
}

/// Cold storage owned by the existing inline mutator's pointer-sized reserve.
/// This is the legacy one-mutator representation, not a parallel registry:
/// a real future registry must own each hot/cold pair as one registration.
struct MutatorSatbRegistry {
    current: SatbMutatorState,
    /// Owner-resident SATB words folded by the last successful termination.
    /// Keep diagnostics in this cold allocation so heap/JIT offsets do not move.
    last_termination_partial_words: usize,
    #[cfg(test)]
    registered: Vec<RegisteredMutatorGc>,
}

#[repr(transparent)]
pub(super) struct MutatorSatbSlot(Box<MutatorSatbRegistry>);

impl std::fmt::Debug for MutatorSatbSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutatorSatbSlot")
            .field("current", &self.0.current)
            .finish_non_exhaustive()
    }
}

static_assertions::assert_not_impl_any!(MutatorSatbSlot: Send, Sync, Clone, Copy);
static_assertions::assert_impl_all!(MutatorSatbSlot: std::fmt::Debug);
const _: () = {
    assert!(std::mem::size_of::<MutatorSatbSlot>() == std::mem::size_of::<std::ptr::NonNull<()>>());
    assert!(
        std::mem::align_of::<MutatorSatbSlot>() == std::mem::align_of::<std::ptr::NonNull<()>>()
    );
    assert!(
        std::mem::size_of::<Option<MutatorSatbSlot>>() == std::mem::size_of::<MutatorSatbSlot>()
    );
};

impl MutatorSatbSlot {
    pub(super) fn new() -> Self {
        Self(Box::new(MutatorSatbRegistry {
            current: SatbMutatorState::new(),
            last_termination_partial_words: 0,
            #[cfg(test)]
            registered: Vec::new(),
        }))
    }

    /// Include this Box and the cfg(test) registry descriptor allocation in
    /// memory_inventory, in addition to logs/hash payloads counted per owner.
    #[cfg(feature = "gc-memory-telemetry")]
    pub(super) fn allocation_bytes(&self) -> usize {
        let bytes = std::mem::size_of::<MutatorSatbRegistry>();
        #[cfg(not(test))]
        {
            bytes
        }
        #[cfg(test)]
        {
            bytes + self.0.registered.capacity() * std::mem::size_of::<RegisteredMutatorGc>()
        }
    }

    /// Terminal abandonment only. Heap storage has already been retained;
    /// destructor fallback cannot take locks, wait, or publish any new work.
    pub(super) fn abandon(&mut self) {
        self.0.current.satb.abandon();
        #[cfg(test)]
        for registration in &mut self.0.registered {
            registration.satb.satb.abandon();
        }
    }
}

pub(super) struct StoppedMutatorGc<'h> {
    pub(super) gc: &'h mut MutatorGcState,
    pub(super) satb: &'h mut SatbMutatorState,
}

impl std::fmt::Debug for StoppedMutatorGc<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoppedMutatorGc")
            .field("bytes_since_gc", &self.gc.bytes_since_gc)
            .field("satb", &self.satb)
            .finish()
    }
}

static_assertions::assert_not_impl_any!(StoppedMutatorGc<'static>: Send, Sync, Clone, Copy);
static_assertions::assert_impl_all!(StoppedMutatorGc<'static>: std::fmt::Debug);

impl TaggedHeap {
    pub(super) fn last_termination_partial_words(&self) -> usize {
        self._satb_layout_reserve.0.last_termination_partial_words
    }

    /// Only the stopped-world termination fold records this diagnostic.
    pub(super) fn record_termination_partial_words(
        &mut self,
        words: usize,
        _: &StoppedMutatorsProof,
    ) {
        self._satb_layout_reserve.0.last_termination_partial_words = words;
    }

    #[inline]
    pub(super) fn current_mutator_satb(&self) -> &SatbMutatorState {
        &self._satb_layout_reserve.0.current
    }

    #[inline]
    pub(super) fn current_mutator_satb_mut(&mut self) -> &mut SatbMutatorState {
        debug_assert_eq!(
            self._satb_layout_reserve.0.current.lifecycle,
            SatbMutatorLifecycle::Active
        );
        &mut self._satb_layout_reserve.0.current
    }

    pub(super) fn satb_parts(&mut self) -> (&mut SatbMutatorState, &SatbHandoff) {
        (&mut self._satb_layout_reserve.0.current, &self.satb_handoff)
    }

    /// Inventory reads the same registrations as the hot iterator family.
    pub(super) fn mutator_satb_states(&self) -> impl Iterator<Item = &SatbMutatorState> {
        let current = std::iter::once(self.current_mutator_satb());
        #[cfg(not(test))]
        {
            current
        }
        #[cfg(test)]
        {
            current.chain(
                self._satb_layout_reserve
                    .0
                    .registered
                    .iter()
                    .map(|entry| &entry.satb),
            )
        }
    }

    /// All noncurrent writes are gated by the closed stopped-world proof.
    pub(super) fn mutator_gc_entries_world_stopped(
        &mut self,
        _: &StoppedMutatorsProof,
    ) -> impl Iterator<Item = StoppedMutatorGc<'_>> {
        let registry = &mut self._satb_layout_reserve.0;
        let current = std::iter::once(StoppedMutatorGc {
            gc: &mut self.mutator_gc,
            satb: &mut registry.current,
        });
        #[cfg(not(test))]
        {
            current
        }
        #[cfg(test)]
        {
            current.chain(
                registry
                    .registered
                    .iter_mut()
                    .map(|entry| StoppedMutatorGc {
                        gc: &mut entry.gc,
                        satb: &mut entry.satb,
                    }),
            )
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub(super) struct TestMutatorGcId(std::num::NonZeroUsize);

#[cfg(test)]
impl TestMutatorGcId {
    fn for_index(index: usize) -> Self {
        Self(
            index
                .checked_add(1)
                .and_then(std::num::NonZeroUsize::new)
                .expect("test mutator registration ID space exhausted"),
        )
    }

    fn index(self) -> usize {
        self.0.get() - 1
    }
}

#[cfg(test)]
impl TaggedHeap {
    /// Only the capability exposes registration to tests; IDs stay valid
    /// through retirement and final fold, so exited logs cannot disappear.
    pub(super) fn register_mutator_gc_for_test(
        &mut self,
        _: &StoppedMutatorsProof,
    ) -> TestMutatorGcId {
        let records = &mut self._satb_layout_reserve.0.registered;
        let id = TestMutatorGcId::for_index(records.len());
        records.push(RegisteredMutatorGc {
            gc: MutatorGcState::new(),
            satb: SatbMutatorState::new(),
        });
        id
    }

    pub(super) fn registered_mutator_gc_for_test(
        &mut self,
        id: TestMutatorGcId,
        _: &StoppedMutatorsProof,
    ) -> StoppedMutatorGc<'_> {
        let entry = &mut self._satb_layout_reserve.0.registered[id.index()];
        StoppedMutatorGc {
            gc: &mut entry.gc,
            satb: &mut entry.satb,
        }
    }
}

// Pin the actual hot physical layout on every x86-64 compilation, including
// unit tests. A new cold field must never silently migrate into this prefix.
#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const _: () = {
    use std::mem::{align_of, offset_of, size_of};
    assert!(size_of::<MutatorGcState>() == 800);
    assert!(align_of::<MutatorGcState>() == 8);
    assert!(offset_of!(MutatorGcState, black_cons_region_start) == 0);
    assert!(offset_of!(MutatorGcState, black_float_region_start) == 16);
    assert!(offset_of!(MutatorGcState, remset) == 32);
    assert!(offset_of!(MutatorGcState, black_born) == 56);
    assert!(offset_of!(MutatorGcState, black_born_regions) == 80);
    assert!(offset_of!(MutatorGcState, major_symbol_preimages) == 104);
    assert!(offset_of!(MutatorGcState, major_cons_writes) == 128);
    assert!(offset_of!(MutatorGcState, r_mapped_seen) == 152);
    assert!(offset_of!(MutatorGcState, allocated_count) == 184);
    assert!(offset_of!(MutatorGcState, memory_use_counts) == 192);
    assert!(offset_of!(MutatorGcState, bytes_since_gc) == 248);
    assert!(offset_of!(MutatorGcState, bytes_banked_at_resets) == 256);
    assert!(offset_of!(MutatorGcState, remembered_cache) == 264);
    assert!(offset_of!(MutatorGcState, pacing) == 776);
};

#[cfg(test)]
#[derive(Debug, thiserror::Error)]
pub(super) enum TestMutatorSwitchError {
    #[error("unknown registered test mutator")]
    Unknown,
    #[error("retired test mutator cannot become a writer")]
    Retired,
}

#[cfg(test)]
struct TestMutatorSwitch<'h> {
    heap: &'h mut TaggedHeap,
    id: TestMutatorGcId,
}

#[cfg(test)]
impl TestMutatorSwitch<'_> {
    /// Refresh the existing heap's owner mirrors without a fallible TLS access.
    /// Same-heap owner switching changes no compiled barrier-window field.
    fn refresh_tls(heap: &mut TaggedHeap) {
        let _ = TAGGED_HEAP.try_with(|slot| slot.set(heap as *mut TaggedHeap));
        let _ = TAGGED_HEAP_ID.try_with(|slot| slot.set(Some(heap.identity())));
        let _ =
            TAGGED_HEAP_WRITE_TRACKING_MODE.try_with(|slot| slot.set(heap.write_tracking_mode()));
        let _ = TAGGED_HEAP_PARTITION_ACTIVE.try_with(|slot| slot.set(heap.partition_dump));
        let _ =
            TAGGED_HEAP_CONCURRENT_ACTIVE.try_with(|slot| slot.set(heap.concurrent_mark_running));
        let _ = TAGGED_HEAP_CONCURRENT_HASH_ACTIVE.try_with(|slot| {
            slot.set(
                heap.concurrent_mark_running
                    && heap.concurrent_claims()
                    && heap.concurrent_hash_snapshot().is_some(),
            )
        });
        let _ =
            TAGGED_HEAP_DUMP_SPAN.try_with(|slot| slot.set((heap.dump_addr_lo, heap.dump_addr_hi)));
        let window = heap.barrier_window();
        let _ = TAGGED_HEAP_BARRIER_WINDOW.try_with(|slot| slot.set(window));
        let _ = TAGGED_HEAP_CONS_BARRIER_WINDOW
            .try_with(|slot| slot.set(heap.cons_barrier_window(window)));
        let _ = TAGGED_HEAP_SATB_CACHE.try_with(|slots| {
            for slot in slots {
                slot.set(0);
            }
        });
        heap.current_mutator_gc_mut().remembered_cache.fill(0);
    }
}

#[cfg(test)]
impl Drop for TestMutatorSwitch<'_> {
    fn drop(&mut self) {
        // Construction validated the ID; no test API removes registrations.
        // get_mut keeps Drop total even if a future test API violates that
        // assumption. There is no allocation, lock, callback or assertion.
        let registry = &mut *self.heap._satb_layout_reserve.0;
        if let Some(entry) = registry.registered.get_mut(self.id.index()) {
            std::mem::swap(&mut self.heap.mutator_gc, &mut entry.gc);
            std::mem::swap(&mut registry.current, &mut entry.satb);
        }
        Self::refresh_tls(self.heap);
    }
}

#[cfg(test)]
impl TaggedHeap {
    /// Called only by SingleMutatorWorld's test wrapper. Run actual barriers
    /// with the selected registration's real hot and SATB state installed.
    /// The closure must not start/finish a GC, allocate Lisp objects, run Lisp,
    /// mutate registry membership, or open allocation regions. Preallocate the
    /// stress graph before using this helper. Rust log-vector growth is fine.
    pub(super) fn with_registered_mutator_gc_for_test<R>(
        &mut self,
        id: TestMutatorGcId,
        _: &StoppedMutatorsProof,
        body: impl FnOnce(&mut TaggedHeap) -> R,
    ) -> Result<R, TestMutatorSwitchError> {
        let registry = &mut *self._satb_layout_reserve.0;
        let entry = registry
            .registered
            .get_mut(id.index())
            .ok_or(TestMutatorSwitchError::Unknown)?;
        if entry.satb.lifecycle == SatbMutatorLifecycle::Retired {
            return Err(TestMutatorSwitchError::Retired);
        }
        std::mem::swap(&mut self.mutator_gc, &mut entry.gc);
        std::mem::swap(&mut registry.current, &mut entry.satb);
        let guard = TestMutatorSwitch { heap: self, id };
        // Guard exists before any TLS call that could unwind. This also clears
        // the SATB repeat-owner cache between two mutators writing one owner.
        set_tagged_heap(guard.heap);
        let result = body(guard.heap);
        drop(guard);
        Ok(result)
    }
}
