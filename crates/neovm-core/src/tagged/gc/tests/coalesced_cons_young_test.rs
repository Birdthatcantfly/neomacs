//! Adjacent allocation grants share only the barrier's handed-young prefix.
//! Allocation, collection, card marking, refunds, and birth logs stay exact.

use super::*;
use crate::tagged::gc::knobs::set_chunk_map_for_test;
use crate::tagged::mutate::{set_cons_car, set_cons_cdr};
use crate::tagged::value::TAG_MASK;

fn installed_heap(chunk_map: bool) -> Box<TaggedHeap> {
    set_chunk_map_for_test(Some(chunk_map));
    let mut heap = Box::new(TaggedHeap::new());
    set_chunk_map_for_test(None);
    heap.set_generational_enabled(true);
    // Keep region grants deterministic; these tests trigger collections
    // explicitly and do not exercise pacing/threshold selection.
    heap.gc_threshold = 1_000_000;
    set_tagged_heap(&mut heap);
    assert_eq!(heap.chunk_map.is_some(), chunk_map);
    heap
}

fn address(value: TaggedValue) -> usize {
    value.bits() & !TAG_MASK
}

fn offset(value: TaggedValue) -> u16 {
    (address(value) & (CONS_BLOCK_ALIGN - 1)) as u16
}

fn cell(heap: &mut TaggedHeap, value: i64) -> TaggedValue {
    heap.alloc_cons(TaggedValue::fixnum(value), TaggedValue::NIL)
}

fn cells(heap: &mut TaggedHeap, count: usize) -> Vec<TaggedValue> {
    (0..count).map(|i| cell(heap, i as i64)).collect()
}

fn minor(heap: &mut TaggedHeap, roots: &[TaggedValue]) {
    heap.begin_minor_collection();
    for &root in roots {
        heap.seed_root(root);
    }
    heap.complete_minor_collection();
    heap.finish_incremental_sweep_now();
    heap.gc_threshold = 1_000_000;
}

fn major(heap: &mut TaggedHeap, roots: &[TaggedValue]) {
    heap.collect_exact(roots.iter().copied());
    heap.finish_incremental_sweep_now();
    heap.gc_threshold = 1_000_000;
}

fn free_addresses(heap: &TaggedHeap) -> Vec<usize> {
    let mut result = Vec::new();
    let mut next = heap.cons_free_list;
    while !next.is_null() {
        assert!(
            result.len() < heap.cons_blocks.len() * CONS_BLOCK_SIZE,
            "free-list cycle or duplicate handout"
        );
        result.push(next as usize);
        // SAFETY: this is the heap's stopped, owned free-list chain.
        next = unsafe { (*next).free_next() };
    }
    result
}

fn counts(heap: &TaggedHeap) -> (u64, usize, usize, usize) {
    (
        heap.memory_use_counts_snapshot()[MemoryUseCountSlot::ConsCells.index()],
        heap.allocated_count(),
        heap.bytes_since_gc_exact(),
        heap.cons_live_count_exact(),
    )
}

fn assert_extra_conses(before: (u64, usize, usize, usize), heap: &TaggedHeap, n: usize) {
    assert_eq!(
        counts(heap),
        (
            before.0 + n as u64,
            before.1 + n,
            before.2 + n * size_of::<ConsCell>(),
            before.3 + n,
        )
    );
}

#[test]
fn coalesced_cons_young_keeps_earlier_owners_across_adjacent_tail_grants() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        // Start above offset zero so retaining the original offset is visible.
        cell(&mut heap, -1);
        heap.close_alloc_regions();
        let before = counts(&heap);
        let refills_before = heap.region_stats.cons_refills;
        let n = 3 * CONS_REGION_MAX_CELLS;
        let fresh = cells(&mut heap, n);
        for pair in fresh.windows(2) {
            assert_eq!(address(pair[1]), address(pair[0]) + size_of::<ConsCell>());
        }
        assert_ne!(offset(fresh[0]), 0);
        assert_eq!(heap.region_book.cons_first_offset, offset(fresh[0]));
        assert_eq!(heap.region_stats.cons_refills - refills_before, 3);
        assert_eq!(heap.cons_region_for_test().0, heap.cons_region_for_test().1);
        assert_extra_conses(before, &heap, n);

        assert!(set_cons_cdr(fresh[0], fresh[n - 1]));
        assert!(!heap.owner_card_is_dirty_for_test(fresh[0]));
        minor(&mut heap, &[fresh[0]]);
        assert_eq!(fresh[0].cons_cdr(), fresh[n - 1]);
        assert_eq!(fresh[n - 1].cons_car(), TaggedValue::fixnum((n - 1) as i64));
    }
}

#[test]
fn coalesced_cons_young_spans_adjacent_reclaimed_free_list_grants() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        let n = 3 * CONS_REGION_MAX_CELLS;
        let original = cells(&mut heap, n + 2);
        minor(&mut heap, &original);
        major(&mut heap, &[original[0], original[n + 1]]);
        let free_before = free_addresses(&heap);
        assert_eq!(free_before.len(), n);
        let before = counts(&heap);
        let refills_before = heap.region_stats.cons_refills;
        let fresh = cells(&mut heap, n);
        assert_eq!(
            fresh.iter().map(|v| address(*v)).collect::<Vec<_>>(),
            free_before
        );
        assert_eq!(heap.region_book.cons, ConsRegionSource::FreeListRun);
        assert_eq!(heap.region_book.cons_first_offset, offset(fresh[0]));
        assert_eq!(heap.region_stats.cons_refills - refills_before, 3);
        assert_extra_conses(before, &heap, n);
        assert!(set_cons_cdr(fresh[0], fresh[n - 1]));
        assert!(!heap.owner_card_is_dirty_for_test(fresh[0]));
        minor(&mut heap, &[fresh[0]]);
        assert_eq!(fresh[0].cons_cdr(), fresh[n - 1]);
        assert_eq!(fresh[n - 1].cons_car(), TaggedValue::fixnum((n - 1) as i64));
    }
}

#[test]
fn coalesced_cons_young_does_not_bridge_an_old_survivor_gap() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        let n = CONS_REGION_MAX_CELLS;
        let original = cells(&mut heap, 2 * n + 3);
        minor(&mut heap, &original);
        let keeper = original[n];
        major(&mut heap, &[keeper, original[2 * n + 2]]);
        assert!(heap.cons_age_is_old_for_test(keeper));

        let first_run = cells(&mut heap, n);
        assert_eq!(address(first_run[0]), address(original[0]));
        let (old_cur, old_limit) = heap.cons_region_for_test();
        assert_eq!(old_cur, old_limit);
        assert_eq!(old_cur, address(keeper));
        let second_run_first = cell(&mut heap, 701);
        assert_eq!(address(second_run_first), address(original[n + 1]));
        assert_ne!(address(second_run_first), old_cur);
        assert_eq!(heap.region_book.cons_first_offset, offset(second_run_first));
        let child = cell(&mut heap, 702);
        assert!(set_cons_car(first_run[0], TaggedValue::T));
        assert!(!heap.owner_card_is_dirty_for_test(first_run[0]));
        assert!(set_cons_cdr(keeper, child));
        assert!(heap.owner_card_is_dirty_for_test(keeper));
        // No other root reaches child. A prefix incorrectly spanning keeper
        // would suppress its card and lose this edge at the real minor.
        minor(&mut heap, &[keeper]);
        assert_eq!(keeper.cons_cdr(), child);
        assert_eq!(child.cons_car(), TaggedValue::fixnum(702));
    }
}

#[test]
fn coalesced_cons_young_resets_at_block_rollover() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        cell(&mut heap, -1);
        heap.close_alloc_regions();
        let first = cell(&mut heap, 801);
        let old_base = address(first) & !(CONS_BLOCK_ALIGN - 1);
        let payload_end = old_base + CONS_CELLS_BYTES;
        while heap.cons_region_for_test().0 < payload_end {
            cell(&mut heap, 802);
        }
        assert_eq!(heap.cons_region_for_test(), (payload_end, payload_end));
        assert_eq!(heap.region_book.cons_first_offset, offset(first));
        assert_ne!(offset(first), 0);
        let next_block = cell(&mut heap, 803);
        assert_ne!(address(next_block) & !(CONS_BLOCK_ALIGN - 1), old_base);
        assert_eq!(heap.cons_blocks.len(), 2);
        assert_eq!(heap.region_book.cons_first_offset, offset(next_block));
        assert_eq!(offset(next_block), 0);
        assert!(set_cons_cdr(first, next_block));
        assert!(!heap.owner_card_is_dirty_for_test(first));
        minor(&mut heap, &[first]);
        assert_eq!(first.cons_cdr(), next_block);
        assert_eq!(next_block.cons_car(), TaggedValue::fixnum(803));
    }
}

#[test]
fn coalesced_cons_young_does_not_survive_real_minor_or_major_close() {
    for chunk_map in [false, true] {
        for use_major in [false, true] {
            let mut heap = installed_heap(chunk_map);
            let original = cells(&mut heap, 2 * CONS_REGION_MAX_CELLS + 1);
            let owner = original[0];
            let last = *original.last().unwrap();
            assert_eq!(heap.region_book.cons_first_offset, offset(owner));
            if use_major {
                major(&mut heap, &[owner, last]);
            } else {
                minor(&mut heap, &[owner, last]);
            }
            assert_eq!(heap.cons_region_for_test(), (0, 0));
            assert_eq!(heap.region_book.cons_first_offset, 0);
            assert!(heap.cons_age_is_old_for_test(owner));
            let child = cell(&mut heap, 901);
            assert_eq!(address(child), address(original[1]));
            assert_eq!(heap.region_book.cons_first_offset, offset(child));
            assert_ne!(offset(child), offset(owner));
            assert!(set_cons_cdr(owner, child));
            assert!(heap.owner_card_is_dirty_for_test(owner));
            minor(&mut heap, &[owner]);
            assert_eq!(owner.cons_cdr(), child);
            assert_eq!(child.cons_car(), TaggedValue::fixnum(901));
        }
    }
}

#[test]
fn coalesced_cons_young_partial_tail_close_refunds_only_current_grant() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        cell(&mut heap, -1);
        heap.close_alloc_regions();
        let before = counts(&heap);
        let n = 2 * CONS_REGION_MAX_CELLS + 1;
        let fresh = cells(&mut heap, n);
        let (cur, lim) = heap.cons_region_for_test();
        assert!(cur < lim);
        assert_eq!(heap.region_book.cons_first_offset, offset(fresh[0]));
        assert_extra_conses(before, &heap, n);
        heap.close_alloc_regions();
        assert_eq!(heap.cons_region_for_test(), (0, 0));
        assert_eq!(heap.region_book.cons_first_offset, 0);
        assert_eq!(heap.cons_blocks[0].next_index as usize, n + 1);
        assert_extra_conses(before, &heap, n);
        let child = cell(&mut heap, 1001);
        assert_eq!(address(child), cur);
        assert_eq!(heap.region_book.cons_first_offset, offset(child));
        assert_ne!(offset(child), offset(fresh[0]));
        assert!(set_cons_cdr(fresh[0], child));
        assert!(!heap.owner_card_is_dirty_for_test(fresh[0]));
        minor(&mut heap, &[fresh[0]]);
        assert_eq!(fresh[0].cons_cdr(), child);
        assert_eq!(child.cons_car(), TaggedValue::fixnum(1001));
    }
}

#[test]
fn coalesced_cons_young_partial_free_list_close_preserves_refund_order() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        let original = cells(&mut heap, 3 * CONS_REGION_MAX_CELLS + 2);
        minor(&mut heap, &original);
        major(&mut heap, &[original[0], *original.last().unwrap()]);
        let free_before = free_addresses(&heap);
        let before = counts(&heap);
        let n = 2 * CONS_REGION_MAX_CELLS + 1;
        let fresh = cells(&mut heap, n);
        let (cur, lim) = heap.cons_region_for_test();
        assert!(cur < lim);
        assert_eq!(heap.region_book.cons_first_offset, offset(fresh[0]));
        assert_extra_conses(before, &heap, n);
        heap.close_alloc_regions();
        assert_eq!(heap.cons_region_for_test(), (0, 0));
        assert_eq!(heap.region_book.cons_first_offset, 0);
        assert_eq!(free_addresses(&heap), free_before[n..].to_vec());
        assert_extra_conses(before, &heap, n);
        let child = cell(&mut heap, 1101);
        assert_eq!(address(child), cur);
        assert_eq!(heap.region_book.cons, ConsRegionSource::FreeListRun);
        assert_eq!(heap.region_book.cons_first_offset, offset(child));
        assert_ne!(offset(child), offset(fresh[0]));
        assert!(set_cons_cdr(fresh[0], child));
        assert!(!heap.owner_card_is_dirty_for_test(fresh[0]));
        minor(&mut heap, &[fresh[0]]);
        assert_eq!(fresh[0].cons_cdr(), child);
        assert_eq!(child.cons_car(), TaggedValue::fixnum(1101));
    }
}

#[test]
fn coalesced_cons_young_does_not_coalesce_black_birth_grant_descriptors() {
    for chunk_map in [false, true] {
        let mut heap = installed_heap(chunk_map);
        let anchor = cell(&mut heap, 1200);
        major(&mut heap, &[anchor]);
        heap.concurrent_begin();
        heap.seed_root(anchor);
        heap.launch_concurrent_mark();
        // This is a real launched worker; heap Drop can finish it on panic.
        assert!(heap.generational.major_in_progress);
        let n = 2 * CONS_REGION_MAX_CELLS + 1;
        let born = cells(&mut heap, n);
        let first_of_last_grant = born[2 * CONS_REGION_MAX_CELLS];
        assert_eq!(heap.region_book.cons_first_offset, offset(born[0]));
        assert_eq!(
            heap.current_mutator_gc().black_cons_region_start,
            Some(address(first_of_last_grant)),
            "black-birth start is this grant, not the barrier's earlier prefix"
        );
        assert!(set_cons_cdr(born[0], born[n - 1]));
        heap.close_alloc_regions();
        let birth_groups: Vec<Vec<TaggedValue>> = heap
            .current_mutator_gc()
            .black_born_regions
            .iter()
            .map(|region| region.cons_owners().collect())
            .collect();
        let logged: Vec<TaggedValue> = birth_groups.iter().flatten().copied().collect();
        // Join before checking snapshots and before promotion/sweep.
        heap.join_concurrent_mark();
        assert_eq!(
            birth_groups.iter().map(Vec::len).collect::<Vec<_>>(),
            [CONS_REGION_MAX_CELLS, CONS_REGION_MAX_CELLS, 1]
        );
        assert_eq!(
            logged, born,
            "each handed cell logged once, no refunded tail"
        );
        assert_eq!(heap.current_mutator_gc().black_cons_region_start, None);
        for &value in &born {
            let ptr = value.as_cons_ptr().unwrap();
            let base = ConsBlock::block_base_for_ptr(ptr);
            let block = heap.cons_block_index_by_base[&base];
            assert!(heap.cons_blocks[block].is_marked_ptr(ptr));
        }
        heap.reseed_runtime_and_remembered_roots();
        heap.seed_root(anchor);
        heap.incremental_drain_all();
        let bytes_before = heap.live_bytes();
        heap.incremental_finish(bytes_before, std::time::Instant::now());
        heap.finish_incremental_sweep_now();
        assert_eq!(born[0].cons_cdr(), born[n - 1]);
        for (index, &value) in born.iter().enumerate() {
            assert!(heap.cons_age_is_old_for_test(value));
            assert_eq!(value.cons_car(), TaggedValue::fixnum(index as i64));
        }
    }
}
