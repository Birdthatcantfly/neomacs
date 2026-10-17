//! Admission/root-source negatives beside the sealed capture constructor.
//! No production factory permits the direct capture access used by this child.

use super::*;
use crate::emacs_core::eval::{
    ThreadingMode, push_scratch_gc_roots, restore_scratch_gc_roots, save_scratch_gc_roots,
};
use crate::emacs_core::intern::intern;
use crate::emacs_core::value::{HashTableTest, HashTableWeakness, LispHashTable};
use crate::tagged::gc::{ParallelHeapEra, RegistrationError};

struct ScratchScope(usize);
impl Drop for ScratchScope {
    fn drop(&mut self) {
        restore_scratch_gc_roots(self.0);
    }
}

#[test]
fn frozen_legacy_capture_rejects_a_foreign_world_keeper() {
    let mut first = Context::new();
    let first_keeper = first.issue_green_baton().unwrap();
    let mut second = Context::new();
    let second_keeper = second.issue_green_baton().unwrap();
    assert_ne!(
        first_keeper.parallel_heap_identity(),
        second_keeper.parallel_heap_identity()
    );
    assert!(matches!(
        first.capture_parallel_legacy_roots(&second_keeper),
        Err(RendezvousError::Identity)
    ));
    assert!(matches!(
        second.capture_parallel_legacy_roots(&first_keeper),
        Err(RendezvousError::Identity)
    ));
    assert!(first_keeper.held_here());
    assert!(second_keeper.held_here());
    drop(second_keeper);
    drop(first_keeper);
}

#[test]
fn rooted_admission_rejects_real_capture_from_another_heap_and_returns_capabilities() {
    let mut first = Context::new();
    first.setup_thread_locals();
    let first_keeper = first.issue_green_baton().unwrap();
    let frozen = first.capture_parallel_legacy_roots(&first_keeper).unwrap();
    let first_heap = frozen.heap_identity();
    // Keep the source Context and keeper alive without running it between
    // capture and the deliberate foreign admission attempt.
    let mut second = Context::new();
    second.setup_thread_locals();
    let keeper = second.issue_green_baton().unwrap();
    let second_heap = keeper.parallel_heap_identity();
    assert_ne!(first_heap, second_heap);
    let heap: *mut crate::tagged::gc::TaggedHeap = &mut *second.tagged_heap;
    // SAFETY: this fresh Context is the sole serial owner; enumeration only
    // seeds its roots, and all capture/identity reads precede the heap loan.
    let collector = unsafe {
        (*heap).drain_to_quiescent(|heap| {
            second.seed_registered_mutator_roots_world_stopped(heap);
        })
    }
    .unwrap();
    let failure = ParallelHeapEra::admit_rooted(collector, keeper, frozen).unwrap_err();
    assert_eq!(failure.reason, RegistrationError::WrongHeap);
    assert_eq!(failure.collector.heap_identity(), second_heap);
    assert_eq!(failure.keeper.parallel_heap_identity(), second_heap);
    assert!(failure.keeper.held_here());
    drop(failure.collector);
    drop(failure.keeper);
    drop(first_keeper);
}

fn unsupported_fixture_refuses_before_registration(make: impl FnOnce(&mut Context) -> TaggedValue) {
    // Clean the same known unsupported GNU default BEFORE the injected
    // fixture object, so weak/native/finalizer negatives are discriminating.
    let mut context = collection_fixture_context();
    assert_fixture_refuses_before_registration(&mut context, make);
}

fn assert_fixture_refuses_before_registration(
    context: &mut Context,
    make: impl FnOnce(&mut Context) -> TaggedValue,
) {
    context.setup_thread_locals();
    let unsupported = make(context);
    // Retain the unsupported object across both serial admission drains so a
    // preflight cannot pass merely because setup garbage was already swept.
    context.lexenv = unsupported;
    let keeper = context.issue_green_baton().unwrap();
    let expected = keeper.parallel_heap_identity();
    let frozen = context.capture_parallel_legacy_roots(&keeper).unwrap();
    let heap: *mut crate::tagged::gc::TaggedHeap = &mut *context.tagged_heap;
    // SAFETY: sole serial Context, no callback/manager loan, private capture
    // completed before this heap loan, and keeper excludes concurrent mark.
    let collector = unsafe {
        (*heap).drain_to_quiescent(|heap| {
            context.seed_registered_mutator_roots_world_stopped(heap);
        })
    }
    .unwrap();
    let failure = ParallelHeapEra::admit_rooted(collector, keeper, frozen).unwrap_err();
    assert_eq!(failure.reason, RegistrationError::Publication);
    assert_eq!(failure.collector.heap_identity(), expected);
    assert_eq!(failure.keeper.parallel_heap_identity(), expected);
    assert!(failure.keeper.held_here());
    // No era was admitted, so no registration/activation could precede this
    // refusal. Both input capabilities return intact for serial recovery.
    drop(failure.collector);
    drop(failure.keeper);
    assert!(context.tagged_heap.owns_heap_value_for_test(unsupported));
    context.lexenv = TaggedValue::NIL;
}

#[test]
fn a5_admission_refuses_weak_hash_fixture_before_any_mutator_activation() {
    unsupported_fixture_refuses_before_registration(|context| {
        context
            .tagged_heap
            .alloc_hash_table(LispHashTable::new_with_options(
                HashTableTest::Eq,
                4,
                Some(HashTableWeakness::Key),
                1.5,
                0.8,
            ))
    });
}

#[test]
fn a5_admission_refuses_native_resource_fixture_even_without_a_finalizer() {
    unsupported_fixture_refuses_before_registration(|context| {
        context
            .tagged_heap
            .alloc_user_ptr(std::ptr::null_mut(), None)
    });
}

#[test]
fn a5_admission_refuses_lisp_finalizer_fixture_before_any_mutator_activation() {
    unsupported_fixture_refuses_before_registration(|context| {
        context.tagged_heap.alloc_finalizer(TaggedValue::T)
    });
}

#[test]
fn a5_capture_refuses_a_preexisting_green_runtime() {
    let mut context = Context::new_with_threading(ThreadingMode::Green);
    let keeper = context.start_green_runtime().unwrap();
    assert!(keeper.held_here());
    assert!(matches!(
        context.capture_parallel_legacy_roots(&keeper),
        Err(RendezvousError::Publication)
    ));
    drop(keeper);
}

#[test]
fn frozen_legacy_capture_includes_own_slab_and_heap_matched_parked_tls_roots() {
    let mut context = Context::new();
    context.setup_thread_locals();
    context
        .eval_str("(defvar p1rv-a5-capture-binding nil)")
        .unwrap();
    let symbol = intern("p1rv-a5-capture-binding");
    context.project_thread_variable_for_test(symbol).unwrap();
    let binding_base = context.specpdl.len();
    let binding = context
        .tagged_heap
        .alloc_cons(TaggedValue::fixnum(5101), TaggedValue::NIL);
    context.try_specbind(symbol, binding).unwrap();
    let scratch = context
        .tagged_heap
        .alloc_cons(TaggedValue::fixnum(5102), TaggedValue::NIL);
    let scratch_scope = ScratchScope(save_scratch_gc_roots());
    push_scratch_gc_roots(&[scratch]);
    let mut private = Vec::new();
    context.trace_private_roots(&mut private);
    assert!(
        !private.contains(&binding),
        "current projected binding must test the separate own-slab source"
    );
    assert!(
        !private.contains(&scratch),
        "scratch must test the separate Parked TLS source"
    );
    let keeper = context.issue_green_baton().unwrap();
    let frozen = context.capture_parallel_legacy_roots(&keeper).unwrap();
    assert_eq!(frozen.heap_identity(), keeper.parallel_heap_identity());
    let mut words = Vec::new();
    frozen.append_words(&mut words);
    assert!(words.contains(&binding));
    assert!(words.contains(&scratch));
    // This is a source-census test, not an admitted era. Destroy its capture
    // before allowing any further Context execution or root mutation.
    drop(frozen);
    drop(keeper);
    drop(scratch_scope);
    context.unbind_to(binding_base);
}

#[cfg(feature = "gc-memory-telemetry")]
#[test]
fn a5_admission_refuses_telemetry_without_unsupported_heap_objects() {
    // A plain cons is supported in ordinary GEN0/GEN1 builds. With telemetry
    // enabled, A5 cannot account for the owner-private Rust buffer inventory,
    // so admission itself must refuse and preserve both serial capabilities.
    unsupported_fixture_refuses_before_registration(|context| {
        context
            .tagged_heap
            .alloc_cons(TaggedValue::fixnum(5103), TaggedValue::NIL)
    });
}

#[test]
fn a5_admission_refuses_full_context_with_gnu_default_weak_table() {
    // Ordinary Context::new correctly includes GNU's weak-value
    // window-dead-windows-table. It remains outside A5's minimal fixture.
    let mut context = Context::new();
    assert_fixture_refuses_before_registration(&mut context, |context| {
        context
            .tagged_heap
            .alloc_cons(TaggedValue::fixnum(5104), TaggedValue::NIL)
    });
}
