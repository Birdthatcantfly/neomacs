//! Packed bias preserves full addresses, colors, and black-tail refund marks.

use super::*;
use crate::tagged::gc::knobs::set_chunk_map_for_test;
use crate::tagged::value::TAG_MASK;

#[test]
fn packed_cons_region_book_round_trips_all_pointer_bits_and_both_colors() {
    let mut book = RegionBook::new();
    assert_eq!(book.cons_young_bias, 0);
    assert_eq!(book.cons_first(), 0);
    assert_eq!(book.cons_color(), RegionColor::White);
    let alignment_mask = size_of::<ConsCell>() - 1;
    let mut starts = vec![0, CONS_BLOCK_ALIGN, usize::MAX & !alignment_mask];
    for shift in 0..usize::BITS {
        starts.push((1usize << shift) & !alignment_mask);
    }
    starts.sort_unstable();
    starts.dedup();
    for first in starts {
        for color in [RegionColor::White, RegionColor::Black] {
            book.set_cons_first_and_color(first, color);
            let color_bit = usize::from(color == RegionColor::Black);
            assert_eq!(book.cons_first(), first);
            assert_eq!(book.cons_color(), color);
            assert_eq!(book.cons_young_bias & 1, color_bit);
            // Translation cancels every full address bit independently.
            assert_eq!(first.wrapping_add(book.cons_young_bias), color_bit);
            book.clear_cons_first();
            assert_eq!(book.cons_first(), 0);
            assert_eq!(book.cons_color(), color);
            assert_eq!(book.cons_young_bias, color_bit);
        }
    }
}

fn counts(heap: &TaggedHeap) -> (u64, usize, usize, usize) {
    (
        heap.memory_use_counts_snapshot()[MemoryUseCountSlot::ConsCells.index()],
        heap.allocated_count(),
        heap.bytes_since_gc_exact(),
        heap.cons_live_count_exact(),
    )
}

fn marked(heap: &TaggedHeap, addr: usize) -> bool {
    let ptr = addr as *const ConsCell;
    let block = heap.cons_block_index_by_base[&ConsBlock::block_base_for_ptr(ptr)];
    heap.cons_blocks[block].is_marked_ptr(ptr)
}

#[test]
fn packed_cons_region_book_black_partial_close_keeps_color_and_unmarks_only_refunded_tail() {
    for chunk_map in [false, true] {
        set_chunk_map_for_test(Some(chunk_map));
        let mut heap = Box::new(TaggedHeap::new());
        set_chunk_map_for_test(None);
        heap.set_generational_enabled(true);
        heap.gc_threshold = 1_000_000;
        set_tagged_heap(&mut heap);
        assert_eq!(heap.chunk_map.is_some(), chunk_map);
        heap.set_sweep_in_progress_for_test(true);
        let before = counts(&heap);
        let refills_before = heap.region_stats.cons_refills;
        let n = 2 * CONS_REGION_MAX_CELLS + 1;
        let handed: Vec<_> = (0..n)
            .map(|i| heap.alloc_cons(TaggedValue::fixnum(i as i64), TaggedValue::NIL))
            .collect();
        let first = handed[0].bits() & !TAG_MASK;
        assert_eq!(heap.region_book.cons_first(), first);
        assert_eq!(heap.region_book.cons_color(), RegionColor::Black);
        assert_eq!(heap.region_stats.cons_refills - refills_before, 3);
        assert!(handed.iter().all(|v| marked(&heap, v.bits() & !TAG_MASK)));
        let (cur, lim) = heap.cons_region_for_test();
        assert!(cur < lim);
        let tail: Vec<_> = (cur..lim).step_by(size_of::<ConsCell>()).collect();
        assert_eq!(tail.len(), CONS_REGION_MAX_CELLS - 1);
        assert!(tail.iter().all(|&a| marked(&heap, a)));
        let exact = (
            before.0 + n as u64,
            before.1 + n,
            before.2 + n * size_of::<ConsCell>(),
            before.3 + n,
        );
        assert_eq!(counts(&heap), exact);

        // Close while the phase is STILL black. The retained color must drive
        // unmarking even after the handed-young address has been cleared.
        heap.close_alloc_regions();
        assert!(heap.sweep_in_progress);
        assert_eq!(heap.cons_region_for_test(), (0, 0));
        assert_eq!(heap.region_book.cons, ConsRegionSource::Closed);
        assert_eq!(heap.region_book.cons_first(), 0);
        assert_eq!(heap.region_book.cons_color(), RegionColor::Black);
        assert_eq!(heap.region_book.cons_young_bias, 1);
        assert_eq!(counts(&heap), exact);
        assert_eq!(heap.cons_blocks[0].next_index as usize, n);
        assert!(handed.iter().all(|v| marked(&heap, v.bits() & !TAG_MASK)));
        assert!(tail.iter().all(|&a| !marked(&heap, a)));

        heap.set_sweep_in_progress_for_test(false);
        let white = heap.alloc_cons(TaggedValue::fixnum(9001), TaggedValue::NIL);
        let white_addr = white.bits() & !TAG_MASK;
        assert_eq!(white_addr, cur);
        assert_eq!(heap.region_book.cons_first(), white_addr);
        assert_eq!(heap.region_book.cons_color(), RegionColor::White);
        assert_eq!(heap.region_book.cons_young_bias & 1, 0);
        assert!(!marked(&heap, white_addr));
        assert_eq!(white.cons_car(), TaggedValue::fixnum(9001));
        assert_eq!(
            handed[n - 1].cons_car(),
            TaggedValue::fixnum((n - 1) as i64)
        );
    }
}
