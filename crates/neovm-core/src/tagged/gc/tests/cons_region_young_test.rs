//! Open allocation regions contain young cells until a collection closes them.

use super::knobs::set_chunk_map_for_test;
use super::*;
use crate::tagged::mutate::{set_cons_car, set_cons_cdr};
use crate::tagged::value::TAG_MASK;

fn heap_with_chunk_map(enabled: bool) -> Box<TaggedHeap> {
    set_chunk_map_for_test(Some(enabled));
    let mut heap = Box::new(TaggedHeap::new());
    set_chunk_map_for_test(None);
    heap.set_generational_enabled(true);
    set_tagged_heap(&mut heap);
    assert_eq!(heap.chunk_map.is_some(), enabled);
    heap
}

fn address(value: TaggedValue) -> usize {
    value.bits() & !TAG_MASK
}

fn block_base(value: TaggedValue) -> usize {
    address(value) & !(CONS_BLOCK_BYTES - 1)
}

fn minor(heap: &mut TaggedHeap, roots: &[TaggedValue]) {
    heap.begin_minor_collection();
    for &root in roots {
        heap.seed_root(root);
    }
    heap.complete_minor_collection();
    heap.finish_incremental_sweep_now();
}

#[cfg(target_pointer_width = "64")]
#[test]
fn cons_region_young_bookkeeping_preserves_existing_footprint() {
    use std::mem::{align_of, offset_of, size_of};

    // The staged R2 shipping binary's DWARF records these old offsets and
    // size. The new offset occupies padding instead of moving heap fields.
    assert_eq!(size_of::<RegionBook>(), 40);
    assert_eq!(align_of::<RegionBook>(), 8);
    assert_eq!(offset_of!(RegionBook, cons), 0);
    assert_eq!(offset_of!(RegionBook, float), 16);
    assert_eq!(offset_of!(RegionBook, cons_color), 32);
    assert_eq!(offset_of!(RegionBook, cons_first_offset), 34);
    assert_eq!(size_of::<JitHeapState>(), 48);
}

#[test]
fn cons_region_young_reopen_keeps_prior_survivor_old_and_child_alive() {
    for chunk_map in [false, true] {
        let mut heap = heap_with_chunk_map(chunk_map);
        let owner = heap.alloc_cons(TaggedValue::fixnum(101), TaggedValue::NIL);
        minor(&mut heap, &[owner]);
        assert!(heap.cons_age_is_old_for_test(owner));
        assert_eq!(heap.cons_region_for_test(), (0, 0));

        let child = heap.alloc_cons(TaggedValue::fixnum(202), TaggedValue::NIL);
        assert_eq!(block_base(owner), block_base(child));
        assert!(address(owner) < address(child));
        assert!(!heap.cons_age_is_old_for_test(child));
        assert!(set_cons_cdr(child, TaggedValue::T));
        assert!(!heap.owner_card_is_dirty_for_test(child));
        assert!(set_cons_cdr(owner, child));
        assert!(heap.owner_card_is_dirty_for_test(owner));

        // Child is reachable only through the old owner's newly written
        // edge, so the card must retain it through this actual minor.
        minor(&mut heap, &[owner]);
        assert_eq!(owner.cons_cdr(), child);
        assert_eq!(child.cons_car(), TaggedValue::fixnum(202));
        assert_eq!(child.cons_cdr(), TaggedValue::T);
        assert!(heap.cons_age_is_old_for_test(child));
        assert!(!heap.owner_card_is_dirty_for_test(owner));
    }
}

#[test]
fn cons_region_young_reclaimed_old_run_excludes_its_live_old_neighbour() {
    for chunk_map in [false, true] {
        let mut heap = heap_with_chunk_map(chunk_map);
        let dead = [11, 22, 33].map(|n| heap.alloc_cons(TaggedValue::fixnum(n), TaggedValue::NIL));
        let keeper = heap.alloc_cons(TaggedValue::fixnum(44), TaggedValue::NIL);
        minor(&mut heap, &[dead[0], dead[1], dead[2], keeper]);
        for value in dead {
            assert!(heap.cons_age_is_old_for_test(value));
        }

        // A real major clears the dead cells' old ages before making them
        // allocatable. Keeper retains this block and remains old.
        heap.collect_exact(std::iter::once(keeper));
        assert!(heap.cons_age_is_old_for_test(keeper));
        let reused = heap.alloc_cons(TaggedValue::NIL, TaggedValue::NIL);
        let child = heap.alloc_cons(TaggedValue::fixnum(303), TaggedValue::NIL);
        assert_eq!(reused.bits(), dead[0].bits());
        assert_eq!(child.bits(), dead[1].bits());
        assert_eq!(heap.region_book.cons, ConsRegionSource::FreeListRun);
        let (cursor, limit) = heap.cons_region_for_test();
        assert!(cursor < limit);
        assert_eq!(limit, address(keeper));
        assert!(!heap.cons_age_is_old_for_test(reused));
        assert!(!heap.cons_age_is_old_for_test(child));

        // The fresh handed cells are young, but the old keeper lies above
        // the handed range in the same block and must still dirty its card.
        assert!(set_cons_car(reused, TaggedValue::T));
        assert!(!heap.owner_card_is_dirty_for_test(reused));
        assert!(set_cons_cdr(keeper, child));
        assert!(heap.owner_card_is_dirty_for_test(keeper));
        minor(&mut heap, &[keeper]);
        assert_eq!(keeper.cons_cdr(), child);
        assert_eq!(child.cons_car(), TaggedValue::fixnum(303));
        assert!(heap.cons_age_is_old_for_test(child));
    }
}
