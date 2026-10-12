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
