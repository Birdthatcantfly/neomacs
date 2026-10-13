//! Card-mark S1: every logged GEN1 owner keeps a dirty card until the
//! stop-all reset that also clears its logged bit.

use super::card_table::{CARD_INDEX_MASK, CARD_SHIFT, CardTable, card_index};
use super::*;
use crate::emacs_core::eval::{
    push_scratch_gc_root, restore_scratch_gc_roots, save_scratch_gc_roots,
};
use crate::emacs_core::value::{HashTableTest, LispHashTable};
use crate::heap_types::LispString;
use std::panic::{AssertUnwindSafe, catch_unwind};

struct ScratchRoots(usize);

impl ScratchRoots {
    fn new() -> Self {
        Self(save_scratch_gc_roots())
    }

    fn keep(&self, value: TaggedValue) {
        push_scratch_gc_root(value);
    }
}

impl Drop for ScratchRoots {
    fn drop(&mut self) {
        restore_scratch_gc_roots(self.0);
    }
}

fn verified_heap() -> TaggedHeap {
    let mut heap = TaggedHeap::new();
    heap.generational.enabled = true;
    heap.generational.verify = true;
    heap.partition_dump = false;
    heap.dump_blackened = false;
    heap.publish_barrier_window();
    heap
}

fn minor(heap: &mut TaggedHeap, roots: &[TaggedValue]) {
    heap.begin_minor_collection();
    for &root in roots {
        heap.seed_root(root);
    }
    heap.complete_minor_collection();
    heap.finish_incremental_sweep_now();
}

fn card_addr(owner: TaggedValue) -> usize {
    TaggedHeap::owner_card_addr(owner).expect("heap owner")
}

#[test]
fn card_index_masks_and_aliases_without_losing_marks() {
    let cards = CardTable::new();
    let base = 0x7f00_0000_0000usize;
    assert_eq!(card_index(base), card_index(base + (1 << CARD_SHIFT) - 1));
    assert_ne!(card_index(base), card_index(base + (1 << CARD_SHIFT)));
    let alias = base + ((CARD_INDEX_MASK + 1) << CARD_SHIFT);
    assert_eq!(card_index(base), card_index(alias));
    assert!(!cards.is_dirty(base));
    cards.mark(alias);
    assert!(
        cards.is_dirty(base),
        "an alias can only make a card look dirty"
    );
    cards.clear(base);
    assert!(!cards.is_dirty(alias));
}

fn dirty_offsets(cards: &CardTable, chunk: usize) -> Vec<usize> {
    cards
        .dirty_chunk(chunk)
        .map_or_else(Vec::new, |cards| cards.dirty_offsets().collect())
}

#[test]
fn chunk_walk_sees_every_card_of_its_chunk_only() {
    use super::card_table::{CARD_BYTES, CHUNK_BYTES};
    let cards = CardTable::new();
    let chunk = 0x7f00_0001_0000usize;
    assert!(dirty_offsets(&cards, chunk).is_empty());
    for offset in [0, CARD_BYTES * 7, CARD_BYTES * 8, CHUNK_BYTES - CARD_BYTES] {
        cards.mark(chunk + offset + 16);
        assert_eq!(dirty_offsets(&cards, chunk), [offset], "offset {offset}");
        assert!(dirty_offsets(&cards, chunk + CHUNK_BYTES).is_empty());
        assert!(dirty_offsets(&cards, chunk - CHUNK_BYTES).is_empty());
        cards.clear(chunk + offset);
        assert!(dirty_offsets(&cards, chunk).is_empty());
    }
}

#[test]
fn chunk_walk_visits_dirty_cards_in_address_order() {
    use super::card_table::{CARD_BYTES, CHUNK_CARDS};
    let cards = CardTable::new();
    let chunk = 0x7f00_0002_0000usize;
    let dirty: Vec<usize> = (0..CHUNK_CARDS)
        .filter(|card| card % 3 == 0 || card % 8 == 7)
        .map(|card| card * CARD_BYTES)
        .collect();
    for &offset in dirty.iter().rev() {
        cards.mark(chunk + offset);
    }
    assert_eq!(dirty_offsets(&cards, chunk), dirty);
}

#[derive(Clone, Copy, Debug)]
enum OwnerClass {
    Cons,
    ArenaVector,
    BoxHashTable,
}

fn store_young_child(owner: TaggedValue, class: OwnerClass, child: TaggedValue) {
    match class {
        OwnerClass::Cons => assert!(crate::tagged::mutate::set_cons_car(owner, child)),
        OwnerClass::ArenaVector => {
            assert!(crate::tagged::mutate::set_vector_slot(owner, 0, child))
        }
        OwnerClass::BoxHashTable => {
            crate::tagged::mutate::with_hash_table_mut(owner, |table| {
                table.insert(
                    TaggedValue::T.to_hash_key(&HashTableTest::Eq),
                    TaggedValue::T,
                    child,
                );
            })
            .expect("hash table owner");
        }
    }
}

fn logged_owner_keeps_a_dirty_card_until_the_reset(class: OwnerClass) {
    let mut heap = verified_heap();
    set_tagged_heap(&mut heap);
    let roots = ScratchRoots::new();
    let owner = match class {
        OwnerClass::Cons => heap.alloc_cons(TaggedValue::NIL, TaggedValue::NIL),
        OwnerClass::ArenaVector => heap.alloc_vector(vec![TaggedValue::NIL]),
        OwnerClass::BoxHashTable => heap.alloc_hash_table(LispHashTable::new(HashTableTest::Eq)),
    };
    roots.keep(owner);
    minor(&mut heap, &[owner]);
    assert!(heap.value_is_old_for_test(owner));
    assert!(!heap.owner_card_is_dirty(card_addr(owner)));

    let child = heap.alloc_string(LispString::from_utf8("card-young-child"));
    store_young_child(owner, class, child);
    assert_eq!(heap.current_mutator_gc().remset, [owner], "{class:?}");
    assert!(heap.owner_card_is_dirty(card_addr(owner)), "{class:?}");

    // Verify mode checks every logged owner's card before promotion.
    minor(&mut heap, &[owner]);
    assert!(
        heap.value_is_old_for_test(child),
        "{class:?}: child survived"
    );
    assert!(
        !heap.owner_card_is_dirty(card_addr(owner)),
        "{class:?}: the reset clears the card with the logged bit"
    );
    store_young_child(
        owner,
        class,
        heap.alloc_cons(TaggedValue::T, TaggedValue::NIL),
    );
    assert!(
        heap.owner_card_is_dirty(card_addr(owner)),
        "{class:?}: re-logged"
    );
}

#[test]
fn logged_old_cons_keeps_a_dirty_card_until_the_reset() {
    logged_owner_keeps_a_dirty_card_until_the_reset(OwnerClass::Cons);
}

#[test]
fn logged_old_arena_vector_keeps_a_dirty_card_until_the_reset() {
    logged_owner_keeps_a_dirty_card_until_the_reset(OwnerClass::ArenaVector);
}

#[test]
fn logged_old_box_owner_keeps_a_dirty_card_until_the_reset() {
    logged_owner_keeps_a_dirty_card_until_the_reset(OwnerClass::BoxHashTable);
}

#[test]
fn verify_mode_rejects_a_logged_owner_with_a_clean_card() {
    let mut heap = verified_heap();
    set_tagged_heap(&mut heap);
    let roots = ScratchRoots::new();
    let owner = heap.alloc_cons(TaggedValue::NIL, TaggedValue::NIL);
    roots.keep(owner);
    minor(&mut heap, &[owner]);
    let child = heap.alloc_cons(TaggedValue::T, TaggedValue::NIL);
    store_young_child(owner, OwnerClass::Cons, child);
    heap.card_table()
        .expect("logging built the table")
        .clear(card_addr(owner));
    heap.begin_minor_collection();
    heap.seed_root(owner);
    let panic = catch_unwind(AssertUnwindSafe(|| heap.complete_minor_collection()))
        .expect_err("a logged owner with a clean card must fail verification");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(message.contains("clean card"), "{message}");
}

#[test]
fn gen0_heap_never_builds_a_card_table() {
    let mut heap = TaggedHeap::new();
    heap.generational.enabled = false;
    heap.publish_barrier_window();
    set_tagged_heap(&mut heap);
    let roots = ScratchRoots::new();
    let owner = heap.alloc_vector(vec![TaggedValue::NIL]);
    roots.keep(owner);
    heap.collect_exact(std::iter::once(owner));
    let child = heap.alloc_cons(TaggedValue::T, TaggedValue::NIL);
    store_young_child(owner, OwnerClass::ArenaVector, child);
    assert!(heap.card_table().is_none());
}

/// S2: the minor traces old owners through their dirty cards, not the R log.
/// An old neighbour in the same card is scanned even though only its
/// sibling was logged; an old cons on a clean card is not a root.
#[test]
fn minor_traces_every_old_cell_of_a_dirty_card() {
    let mut heap = verified_heap();
    set_tagged_heap(&mut heap);
    let roots = ScratchRoots::new();
    let logged = heap.alloc_cons(TaggedValue::NIL, TaggedValue::NIL);
    let neighbour = heap.alloc_cons(TaggedValue::NIL, TaggedValue::NIL);
    roots.keep(logged);
    roots.keep(neighbour);
    minor(&mut heap, &[logged, neighbour]);
    assert_eq!(
        card_index(card_addr(logged)),
        card_index(card_addr(neighbour))
    );
    assert!(heap.value_is_old_for_test(neighbour));

    let logged_child = heap.alloc_string(LispString::from_utf8("logged-child"));
    store_young_child(logged, OwnerClass::Cons, logged_child);
    // An unbarriered store into the neighbour: only the shared card covers it.
    let hidden_child = heap.alloc_string(LispString::from_utf8("card-only-child"));
    unsafe { (*(neighbour.xcons_ptr() as *mut ConsCell)).set_car(hidden_child) };
    assert_eq!(heap.current_mutator_gc().remset, [logged]);

    // The verifying minor would panic if the neighbour's child were missed.
    minor(&mut heap, &[logged, neighbour]);
    assert!(
        heap.value_is_old_for_test(hidden_child),
        "the dirty card scanned the neighbour"
    );
    assert!(heap.value_is_old_for_test(logged_child));
    assert!(!heap.owner_card_is_dirty(card_addr(logged)));
}

/// S2: the old-bitmap word selection reads each card's own half of its
/// word. Only the cells of odd 32-cell cards (plus the last allocated cell)
/// stay old, so a card reading its neighbour's half finds no old cell there
/// and the verifying minor loses their young children.
#[test]
fn minor_reaches_young_children_of_every_card_of_a_block() {
    let mut heap = verified_heap();
    set_tagged_heap(&mut heap);
    let roots = ScratchRoots::new();
    let cells: Vec<TaggedValue> = (0..150)
        .map(|_| heap.alloc_cons(TaggedValue::NIL, TaggedValue::NIL))
        .collect();
    let owners: Vec<TaggedValue> = cells
        .iter()
        .copied()
        .filter(|&cell| {
            let index = ConsBlock::index_of_ptr(cell.xcons_ptr());
            (index / 32) % 2 == 1 || cell.bits() == cells[149].bits()
        })
        .collect();
    assert!(owners.len() > 32);
    for &owner in &owners {
        roots.keep(owner);
    }
    minor(&mut heap, &owners);
    let children: Vec<TaggedValue> = owners
        .iter()
        .map(|&owner| {
            let child = heap.alloc_cons(TaggedValue::T, TaggedValue::NIL);
            store_young_child(owner, OwnerClass::Cons, child);
            child
        })
        .collect();
    minor(&mut heap, &owners);
    for (i, &child) in children.iter().enumerate() {
        assert!(heap.value_is_old_for_test(child), "owner {i}");
    }
}
