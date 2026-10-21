//! Root-owning heap fixture for tests that alternate direct and TLS access.
//!
//! The owner, every view, and TLS all derive from AliasableBox's single root.
//! A helper that crosses TLS/barriers must borrow TestHeap, not TaggedHeap:
//! keeping a payload loan alive across a root-derived sibling is still invalid.

use super::*;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

pub(crate) struct TestHeap {
    owner: AliasableBox<TaggedHeap>,
}

impl TestHeap {
    pub(crate) fn new() -> Self {
        Self::from_heap(TaggedHeap::new())
    }

    /// Finish address-independent setup before installing a custom heap.
    pub(crate) fn from_heap(heap: TaggedHeap) -> Self {
        let fixture = Self {
            owner: AliasableBox::new(heap),
        };
        fixture.install();
        fixture
    }

    /// Copy the provenance root without borrowing any heap bytes.
    pub(crate) fn root(&self) -> NonNull<TaggedHeap> {
        self.owner.root()
    }

    /// Refresh TLS from the root after a phase/barrier-window change.
    pub(crate) fn install(&self) {
        set_tagged_heap_root(self.root());
    }

    pub(in crate::tagged::gc) fn with_mutator_gc_for_test<R>(
        &mut self,
        id: mutator_gc::TestMutatorGcId,
        body: impl FnOnce(NonNull<TaggedHeap>) -> R,
    ) -> Result<R, mutator_gc::TestMutatorSwitchError> {
        self.owner.with_mutator_gc_for_test(id, body)
    }
}

impl Deref for TestHeap {
    type Target = TaggedHeap;

    fn deref(&self) -> &TaggedHeap {
        self.owner.get()
    }
}

impl DerefMut for TestHeap {
    fn deref_mut(&mut self) -> &mut TaggedHeap {
        self.owner.get_mut()
    }
}

#[test]
fn heapborrow_r7_fixture_owner_and_tls_share_root() {
    let mut heap = TestHeap::new();
    let first = heap.alloc_cons(TaggedValue::fixnum(61), TaggedValue::NIL);
    let second = TaggedValue::cons(first, TaggedValue::NIL);
    heap.alloc_vector(vec![second]);
    heap.install();
    let third = TaggedValue::cons(second, TaggedValue::NIL);
    assert_eq!(
        third.cons_car().cons_car().cons_car(),
        TaggedValue::fixnum(61)
    );
    assert!(tagged_heap_is_current(&heap));
    drop(heap);
    assert!(!tagged_heap_is_installed());
}

#[test]
fn heapborrow_r7_displaced_fixture_drop_preserves_new_installation() {
    let first = TestHeap::new();
    let second = TestHeap::new();
    drop(first);
    assert!(tagged_heap_is_current(&second));
    drop(second);
    assert!(!tagged_heap_is_installed());
}
