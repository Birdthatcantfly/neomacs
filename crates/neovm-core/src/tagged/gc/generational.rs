//! Collector state and the per-cycle remembered-set protocol (P3.1 G2).
//! Mutator-owned logs and caches are reached through MutatorGcState.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GenerationCycle {
    Major,
    Minor,
}

pub(super) struct GenState {
    pub(super) enabled: bool,
    /// Constructor-frozen verification; inactive even if requested with GEN0.
    pub(super) verify: bool,
    pub(super) cycle: GenerationCycle,
    /// Full scope lasts until every deferred sweep cursor completes.
    pub(super) major_in_progress: bool,
    pub(super) old_sweep_pending: *mut GcHeader,
    /// Newly traced young headers, including weak/finalizer fixpoints.
    pub(super) promo: Vec<*mut GcHeader>,
    /// Collector-owned residual Box old list; links use GcHeader accessors.
    pub(super) old_objects: *mut GcHeader,
    pub(super) old_bytes: usize,
    /// Constructor-read limits and exact ordinary-old bytes after a major.
    pub(super) pacing_knobs: knobs::GenerationalPacingKnobs,
    pub(super) old_bytes_after_major: usize,
    pub(super) old_cons_count: usize,
    /// Working R: drained at the stop-all boundary, rooted until termination.
    pub(super) r_seed: Vec<TaggedValue>,
}

impl GenState {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            verify: enabled && std::env::var("NEOVM_GC_VERIFY_GENERATIONAL").as_deref() == Ok("1"),
            cycle: GenerationCycle::Major,
            major_in_progress: false,
            old_sweep_pending: std::ptr::null_mut(),
            promo: Vec::new(),
            old_objects: std::ptr::null_mut(),
            old_bytes: 0,
            pacing_knobs: knobs::generational_pacing_knobs(enabled),
            old_bytes_after_major: 0,
            old_cons_count: 0,
            r_seed: Vec::new(),
        }
    }
}

impl TaggedHeap {
    #[inline]
    pub(crate) fn generational_enabled(&self) -> bool {
        self.generational.enabled
    }

    #[inline]
    pub(super) fn old_cons_trailer(
        &self,
        owner: TaggedValue,
    ) -> Option<(&ConsBlockTrailer, usize)> {
        if !owner.is_cons() || self.owner_is_mapped(owner) {
            return None;
        }
        let ptr = owner.xcons_ptr();
        if !ConsBlock::ptr_is_cell_aligned(ptr) {
            return None;
        }
        let base = ConsBlock::block_base_for_ptr(ptr);
        let block = match self.chunk_map.as_ref() {
            Some(map) => {
                let entry = map.get(ptr as usize);
                entry
                    .is(ChunkClass::Cons)
                    .then(|| &self.cons_blocks[entry.index()])
            }
            None => self
                .cons_block_index_by_base
                .get(&base)
                .map(|&i| &self.cons_blocks[i]),
        }?;
        Some((block.trailer(), ConsBlock::index_of_ptr(ptr)))
    }

    /// Called with every mutator stopped and every barrier append complete.
    /// Mapped dedup is local to each mutator; this drain deduplicates across
    /// all registered mutators. Today the iterator yields exactly one.
    #[cold]
    #[inline(never)]
    pub(super) fn seed_generational_remembered(&mut self) {
        if !self.generational.enabled {
            return;
        }
        self.publish_persistent_remembered_world_stopped();
        debug_assert!(self.generational.r_seed.is_empty());
        let mut owners = Vec::new();
        let mut seen = FxHashSet::default();
        for mutator in self.mutators_mut() {
            mutator.remembered_cache.fill(0);
            for owner in mutator.remset.drain(..) {
                if seen.insert(owner.bits()) {
                    owners.push(owner);
                }
            }
        }
        self.generational.r_seed = owners;
        // No Lisp allocation or safepoint inside enumeration; R_seed is
        // collector-owned and remains rooted throughout the cycle. Owners in
        // cons blocks and arena pages are traced through their dirty cards;
        // Box and mapped owners keep the per-owner path.
        for i in 0..self.generational.r_seed.len() {
            let owner = self.generational.r_seed[i];
            if !self.owner_is_card_scanned(owner) {
                self.push_value_children_to_gray(owner, "generational-remembered");
            }
        }
        self.trace_dirty_card_owners_world_stopped();
    }

    /// Merge current-mutator persistent additions only under collector
    /// exclusion. A future stop-all handshake must precede this drain.
    #[cold]
    #[inline(never)]
    pub(super) fn publish_persistent_remembered_world_stopped(&mut self) {
        if !self.generational.enabled {
            return;
        }
        let mut additions = Vec::new();
        for mutator in self.mutators_mut() {
            additions.extend(mutator.r_mapped_seen.drain());
        }
        self.mapped_remembered.extend(additions);
    }

    /// Reset claims before a major traces old objects, and at termination
    /// after P-all. No owner is freed until this drain is complete. Facts
    /// appended during the later cooperative sweep belong to the next cycle.
    #[cold]
    #[inline(never)]
    pub(super) fn reset_generational_remembered_world_stopped(&mut self) {
        if !self.generational.enabled {
            return;
        }
        self.publish_persistent_remembered_world_stopped();
        let mut owners = std::mem::take(&mut self.generational.r_seed);
        for mutator in self.mutators_mut() {
            owners.append(&mut mutator.remset);
            mutator.remembered_cache.fill(0);
        }
        for owner in owners {
            if self.owner_is_mapped(owner) {
                continue;
            }
            if let Some(addr) = Self::owner_card_addr(owner) {
                self.clear_owner_card(addr);
            }
            if owner.is_cons() {
                if let Some((trailer, index)) = self.old_cons_trailer(owner)
                    && trailer.is_old(index)
                {
                    trailer.set_unlogged(index);
                }
            } else if let Some(addr) = Self::value_heap_addr(owner) {
                // All appenders are stopped, and no header has been freed.
                unsafe { &*(addr as *const GcHeader) }
                    .remembered
                    .store(RememberedState::Unlogged as u8, Ordering::Relaxed);
            }
        }
    }

    #[cfg(debug_assertions)]
    pub(super) fn debug_assert_remembered_membership(&self, owner: TaggedValue) {
        debug_assert!(
            self.remembered_membership_is_valid(owner),
            "remembered owner absent from R, R_seed and mapped_remembered: {owner:?}"
        );
    }

    /// Exercise the same invariant in tests even when debug assertions are off.
    #[cfg(test)]
    pub(super) fn assert_remembered_membership_for_test(&self, owner: TaggedValue) {
        assert!(
            self.remembered_membership_is_valid(owner),
            "remembered owner absent from R, R_seed and mapped_remembered: {owner:?}"
        );
    }

    #[cfg(any(test, debug_assertions))]
    fn remembered_membership_is_valid(&self, owner: TaggedValue) -> bool {
        if owner.is_cons() || self.owner_is_mapped(owner) {
            return true;
        }
        if let Some(addr) = Self::value_heap_addr(owner) {
            let header = unsafe { &*(addr as *const GcHeader) };
            !header.is_remembered()
                || self.mapped_remembered.contains(&owner.bits())
                || self
                    .generational
                    .r_seed
                    .iter()
                    .any(|value| value.bits() == owner.bits())
                || self
                    .mutators()
                    .any(|m| m.remset.iter().any(|value| value.bits() == owner.bits()))
        } else {
            true
        }
    }
}

impl TaggedHeap {
    /// Future global stop-all-mutators handshake belongs before this entry:
    /// join the collector, park every mutator and wait for all barrier appends.
    #[cold]
    #[inline(never)]
    pub(crate) fn begin_minor_collection(&mut self) {
        assert!(self.generational.enabled);
        assert!(!self.concurrent_mark_running);
        self.generational.cycle = GenerationCycle::Minor;
        self.generational.promo.clear();
        self.incremental_mark_us = 0;
        self.pace_mark_start = None;
        self.pace_mark_start_bytes = self.bytes_since_gc();
        self.begin_collection_with(false);
        self.mark_in_progress = true;
    }

    /// Marking and all fixpoints execute on the stopped mutator, then eager
    /// promotion arms the ordinary cooperative sweep. No mutator runs between
    /// tracing a survivor and setting its generation bit.
    #[cold]
    #[inline(never)]
    pub(crate) fn complete_minor_collection(&mut self) {
        assert!(self.generational.cycle == GenerationCycle::Minor);
        self.close_alloc_regions();
        let bytes_before = self.live_bytes;
        self.incremental_drain_all();
        self.incremental_finish(bytes_before, std::time::Instant::now());
    }

    #[inline]
    pub(super) fn is_minor_collection(&self) -> bool {
        self.generational.cycle == GenerationCycle::Minor
    }

    #[inline]
    pub(super) fn note_minor_survivor(&mut self, header: *mut GcHeader) {
        if self.generational.enabled && !unsafe { (*header).tenured } {
            self.generational.promo.push(header);
        }
    }

    #[inline]
    pub(super) fn is_generational_major_marking(&self) -> bool {
        self.generational.major_in_progress && self.mark_in_progress
    }

    /// P-all: traced headers plus every mutator's black births. The marker
    /// has joined; no mutator can publish a store until this finishes.
    #[cold]
    #[inline(never)]
    pub(super) fn promote_survivors_world_stopped(&mut self) {
        if !self.generational.enabled {
            return;
        }
        if self.is_minor_collection() && self.generational.verify {
            // Every mutator is stopped and the weak/finalizer fixpoints are
            // complete. Check before promotion could hide a missing mark and
            // before any sweep can reclaim the missed young child.
            self.verify_remembered_owner_cards_world_stopped();
            self.verify_dump_partition();
        }
        let partition_first = self.partition_dump && !self.dump_blackened;
        let mut headers = std::mem::take(&mut self.generational.promo);
        for mutator in self.mutators() {
            headers.extend(mutator.black_born.iter().copied());
            for region in &mutator.black_born_regions {
                headers.extend(region.float_headers());
            }
        }
        let mut newly_promoted_header_bytes = 0usize;
        // The first partition's existing permanent splice owns non-cons
        // promotion, including survivors of a late-loaded image.
        if !partition_first {
            for ptr in headers {
                let header = unsafe { &mut *ptr };
                if !header.tenured {
                    debug_assert!(header.is_marked_at(self.mark_parity));
                    header.tenured = true;
                    let bytes = Self::object_bytes_from_header(ptr);
                    self.generational.old_bytes = self.generational.old_bytes.saturating_add(bytes);
                    newly_promoted_header_bytes = newly_promoted_header_bytes.saturating_add(bytes);
                }
            }
        }
        let major = self.generational.major_in_progress;
        let mut promoted = 0;
        let mut old_count = 0;
        for block in &self.cons_blocks {
            promoted += if major {
                block.promote_major_world_stopped()
            } else {
                block.promote_marked_world_stopped()
            };
            old_count += block.count_old();
        }
        self.generational.old_cons_count = old_count;
        let newly_promoted_cons_bytes = promoted.saturating_mul(size_of::<ConsCell>());
        self.generational.old_bytes = self
            .generational
            .old_bytes
            .saturating_add(newly_promoted_cons_bytes);
        self.record_generation_promoted_bytes(
            newly_promoted_header_bytes.saturating_add(newly_promoted_cons_bytes),
        );
        #[cfg(feature = "gc-memory-telemetry")]
        memory_telemetry::note_promotion(
            self,
            newly_promoted_header_bytes.saturating_add(newly_promoted_cons_bytes),
        );
        if !partition_first {
            self.clear_black_births_world_stopped();
        }
    }

    /// Clear only after ordinary P-all or the first permanent splice. GEN-4
    /// prevents a new collection until deferred first-partition promotion ends.
    #[cold]
    #[inline(never)]
    pub(super) fn clear_black_births_world_stopped(&mut self) {
        for mutator in self.mutators_mut() {
            debug_assert!(mutator.black_cons_region_start.is_none());
            debug_assert!(mutator.black_float_region_start.is_none());
            mutator.black_born.clear();
            mutator.black_born_regions.clear();
        }
    }

    #[inline]
    pub(super) fn cons_block_live_count(&self, block: &ConsBlock) -> usize {
        if self.generational.enabled {
            block.count_live_generational()
        } else {
            block.count_marked()
        }
    }

    #[inline]
    pub(super) fn old_noncons_bytes(&self) -> usize {
        self.generational
            .old_bytes
            .saturating_sub(self.generational.old_cons_count * size_of::<ConsCell>())
    }

    /// Add a promoted Box survivor to the old list under collector exclusion.
    #[inline]
    pub(super) unsafe fn link_old_object_world_stopped(&mut self, ptr: *mut GcHeader) {
        let header = unsafe { &mut *ptr };
        debug_assert!(header.tenured && !header.generation.permanent());
        header.marked.store(UNMARKED_AT_REST, Ordering::Relaxed);
        header.set_gc_link_world_stopped(self.generational.old_objects);
        self.generational.old_objects = ptr;
    }

    #[cfg(test)]
    pub(crate) fn value_is_old_for_test(&self, value: TaggedValue) -> bool {
        if let Some((trailer, index)) = self.old_cons_trailer(value) {
            trailer.is_old(index)
        } else {
            self.value_is_tenured(value)
        }
    }

    #[cfg(test)]
    pub(crate) fn remembered_log_len_for_test(&self) -> usize {
        self.mutators().map(|m| m.remset.len()).sum()
    }
}

impl TaggedHeap {
    /// The address whose card covers OWNER: a cons cell or an object header.
    /// Mapped owners stay on the window path and the persistent mapped set.
    #[inline]
    pub(super) fn owner_card_addr(owner: TaggedValue) -> Option<usize> {
        if owner.is_cons() {
            Some(owner.xcons_ptr() as usize)
        } else {
            Self::value_heap_addr(owner)
        }
    }

    /// The GEN1 card table, carried with the heap's other optional cold
    /// state so that it never widens the inline heap layout.
    #[inline]
    pub(super) fn card_table(&self) -> Option<&CardTable> {
        self.census
            .as_deref()
            .and_then(|carrier| carrier.cards.as_ref())
    }

    /// Mark the card of a newly logged owner. Mutator side, GEN1 only; the
    /// first mark builds the carrier and the table.
    #[inline]
    pub(super) fn mark_owner_card(&mut self, owner_addr: usize) {
        self.census
            .get_or_insert_with(|| Box::new(GenCensus::disabled()))
            .cards
            .get_or_insert_with(CardTable::new)
            .mark(owner_addr);
    }

    #[inline]
    pub(super) fn owner_card_is_dirty(&self, owner_addr: usize) -> bool {
        self.card_table()
            .is_some_and(|cards| cards.is_dirty(owner_addr))
    }

    /// Clear with every mutator stopped, together with the owner's logged bit:
    /// a logged owner always keeps a dirty card until its claim is reset.
    #[inline]
    fn clear_owner_card(&self, owner_addr: usize) {
        if let Some(cards) = self.card_table() {
            cards.clear(owner_addr);
        }
    }

    /// Whether a minor reaches logged OWNER's children through its dirty
    /// card: an old cons in an owned block, or a page-arena object. Box and
    /// mapped owners stay on the remembered-set path. Only `remember_owner`
    /// logs, and it logs an unmapped cons only through its block trailer and
    /// a header only once tenured, which an owned object alone can be; the
    /// `boxed` flag then separates Box objects from arena slots.
    #[inline]
    pub(super) fn owner_is_card_scanned(&self, owner: TaggedValue) -> bool {
        if self.owner_is_mapped(owner) {
            return false;
        }
        if owner.is_cons() {
            debug_assert!(self.old_cons_trailer(owner).is_some());
            return true;
        }
        let Some(addr) = Self::value_heap_addr(owner) else {
            return false;
        };
        debug_assert!(self.owns_heap_value_object(owner, addr));
        // SAFETY: a logged owner is live, so its header is readable.
        !unsafe { &*(addr as *const GcHeader) }.flags.boxed()
    }

    /// Trace the old objects that start in a dirty card: every old cell of a
    /// cons block (32 per card, two children each) and the logged tenured
    /// slots of every arena page (retired pages included), straight onto the
    /// gray stack. Each chunk's cards are read
    /// once and copied only when one is dirty; blocks and pages are re-borrowed
    /// per owner, so nothing is collected first. With every mutator stopped;
    /// an aliased card only adds owners.
    #[cold]
    #[inline(never)]
    fn trace_dirty_card_owners_world_stopped(&mut self) {
        if self.card_table().is_none() {
            return;
        }
        for block in 0..self.cons_blocks.len() {
            let Some(chunk) = self.dirty_chunk(self.cons_blocks[block].base_addr()) else {
                continue;
            };
            for offset in chunk.dirty_offsets() {
                let (first, mut old) = self.cons_blocks[block].old_cells_in_card(offset);
                while old != 0 {
                    let index = first + old.trailing_zeros() as usize;
                    old &= old - 1;
                    // SAFETY: an old cell of an owned block is a live cons.
                    let children = unsafe {
                        let cell = self.cons_blocks[block].cells_ptr().add(index);
                        [(*cell).load_car(), (*cell).load_cdr()]
                    };
                    for child in children {
                        // An old cell is black in a minor: skip it unpushed.
                        if !self.cons_blocks[block].holds_old_cell(child) {
                            self.mark_or_push_child(child, "generational-card");
                        }
                    }
                }
            }
        }
        // Floats and bignums hold no Lisp references.
        self.trace_arena_dirty_cards(|heap| &heap.string_arena);
        self.trace_arena_dirty_cards(|heap| &heap.vector_arena);
        self.trace_arena_dirty_cards(|heap| &heap.bytecode_arena);
        self.trace_arena_dirty_cards(|heap| &heap.lambda_arena);
        self.trace_arena_dirty_cards(|heap| &heap.macro_arena);
        self.trace_arena_dirty_cards(|heap| &heap.record_arena);
        self.trace_arena_dirty_cards(|heap| &heap.symbol_with_pos_arena);
        self.trace_arena_dirty_cards(|heap| &heap.marker_arena);
    }

    #[inline]
    fn dirty_chunk(&self, chunk_base: usize) -> Option<ChunkCards> {
        self.card_table()?.dirty_chunk(chunk_base)
    }

    #[inline]
    fn trace_arena_dirty_cards<T: PagedObject>(&mut self, arena: fn(&Self) -> &ObjectArena<T>) {
        for page in 0..arena(self).pages.len() {
            let Some(chunk) = self.dirty_chunk(arena(self).pages[page].base_addr()) else {
                continue;
            };
            for offset in chunk.dirty_offsets() {
                for slot in ObjectArena::<T>::slots_starting_in_card(offset) {
                    let Some(header) = arena(self).logged_slot_header(page, slot) else {
                        continue;
                    };
                    // SAFETY: a tenured allocated slot holds a live object.
                    let owner = match unsafe { (*header).kind } {
                        HeapObjectKind::VecLike => unsafe {
                            TaggedValue::from_veclike_ptr(header as *const VecLikeHeader)
                        },
                        HeapObjectKind::String => unsafe {
                            TaggedValue::from_string_ptr(header as *const StringObj)
                        },
                        HeapObjectKind::Float => continue,
                    };
                    self.push_value_children_to_gray(owner, "generational-card");
                }
            }
        }
    }

    /// S1 invariant: every unmapped owner in R_seed or a mutator R log has a
    /// dirty card. Runs in GEN1 verify mode with every mutator stopped.
    #[cold]
    #[inline(never)]
    pub(super) fn verify_remembered_owner_cards_world_stopped(&self) {
        let logged = self
            .generational
            .r_seed
            .iter()
            .chain(self.mutators().flat_map(|mutator| mutator.remset.iter()));
        for &owner in logged {
            if self.owner_is_mapped(owner) {
                continue;
            }
            if let Some(addr) = Self::owner_card_addr(owner) {
                assert!(
                    self.owner_card_is_dirty(addr),
                    "GC verify: remembered owner {owner:?} has a clean card"
                );
            }
        }
    }
}
