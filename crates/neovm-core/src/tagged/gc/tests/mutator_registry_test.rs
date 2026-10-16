//! Integration fixtures for actual K hot/cold MutatorGcState registrations.

use super::mutator_gc::{SatbDoneDecision, SatbTerminationObligation};
use super::*;

#[test]
fn satb_registered_hot_and_cold_mutators_fold_retired_owner_and_union_dedup() {
    let mut heap = TaggedHeap::new();
    let handoff = heap.satb_handoff.clone();
    heap.current_mutator_gc_mut().bytes_since_gc = 7;
    heap.current_mutator_satb_mut()
        .satb
        .push(MarkWord::of(TaggedValue::fixnum(0)), &handoff);
    heap.current_mutator_satb_mut()
        .satb_snapshotted_owners
        .insert(17);
    // SAFETY: this deterministic fixture owns all registered mutators on one
    // thread, has no worker/TLS aliases/callbacks and retains its whole heap.
    let mut world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&mut heap) };
    let ids: Vec<_> = (0..3)
        .map(|_| world.register_mutator_gc_for_test())
        .collect();
    for (ordinal, id) in ids.iter().copied().enumerate() {
        let entry = world.mutator_gc_for_test(id);
        entry.gc.bytes_since_gc = ordinal + 1;
        // Every mutator logs the same owner independently; only the stopped
        // union may dedup across owners. This catches accidental shared sets.
        assert!(entry.satb.satb_snapshotted_owners.insert(17));
        entry.satb.satb.push(
            MarkWord::of(TaggedValue::fixnum(((ordinal + 1) * 10) as i64)),
            &handoff,
        );
    }
    world
        .retire_mutator_gc_for_test(ids[1])
        .unwrap()
        .acknowledge();
    for (ordinal, id) in ids.iter().copied().enumerate() {
        if ordinal != 1 {
            world.mutator_gc_for_test(id).satb.satb.push(
                MarkWord::of(TaggedValue::fixnum(((ordinal + 1) * 10 + 1) as i64)),
                &handoff,
            );
        }
    }
    assert_eq!(world.heap().mutators().count(), 4);
    assert_eq!(world.heap().bytes_since_gc(), 13);
    assert_eq!(world.heap().mutator_satb_states().count(), 4);
    assert_eq!(world.fold_satb().unwrap(), 6);
    let owners = world.take_satb_written_owners();
    assert_eq!(owners.len(), 1);
    assert!(owners.contains(&17));
    world.ensure_satb_empty().unwrap();
    let mut words: Vec<_> = world
        .heap()
        .gray_queue
        .iter()
        .map(|v| v.as_fixnum().unwrap())
        .collect();
    words.sort_unstable();
    assert_eq!(words, vec![0, 10, 11, 20, 30, 31]);
}

#[test]
fn satb_begin_rejects_residual_in_any_registration_without_clearing_it() {
    let mut heap = TaggedHeap::new();
    let handoff = heap.satb_handoff.clone();
    // SAFETY: fixture owns all K registered mutator states and the heap.
    let mut world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&mut heap) };
    let id = world.register_mutator_gc_for_test();
    world
        .mutator_gc_for_test(id)
        .satb
        .satb
        .push(MarkWord::of(TaggedValue::fixnum(42)), &handoff);
    assert!(matches!(
        world.begin_satb_cycle(),
        Err(scan_contract::SatbDrainError::Residual)
    ));
    assert_eq!(
        world
            .mutator_gc_for_test(id)
            .satb
            .satb
            .storage_usage()
            .words,
        1
    );
    assert_eq!(world.fold_satb().unwrap(), 1);
    world.begin_satb_cycle().unwrap();
}

#[test]
fn satb_done_poll_flushes_at_most_once_and_resets_on_next_epoch() {
    let mut heap = TaggedHeap::new();
    let handoff = heap.satb_handoff.clone();
    let current = heap.current_mutator_satb_mut();
    current
        .satb
        .push(MarkWord::of(TaggedValue::fixnum(1)), &handoff);
    assert_eq!(
        current.publish_at_done(&handoff).unwrap(),
        SatbDoneDecision::DeferOnce
    );
    current
        .satb
        .push(MarkWord::of(TaggedValue::fixnum(2)), &handoff);
    assert_eq!(
        current.publish_at_done(&handoff).unwrap(),
        SatbDoneDecision::Finish
    );
    assert_eq!(
        current.termination_obligation(),
        SatbTerminationObligation::FinishAtNextSafepoint
    );

    // SAFETY: no worker or other writer exists; this is the stopped fold.
    let mut world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&mut heap) };
    assert_eq!(world.fold_satb().unwrap(), 2);
    world.begin_satb_cycle().unwrap();
    drop(world);
    let current = heap.current_mutator_satb_mut();
    current
        .satb
        .push(MarkWord::of(TaggedValue::fixnum(3)), &handoff);
    assert_eq!(
        current.publish_at_done(&handoff).unwrap(),
        SatbDoneDecision::DeferOnce
    );
    // Clean the handoff without requiring or changing collector mark flags.
    for chunk in handoff.take_all().unwrap() {
        drop(chunk.into_words());
    }
}

#[cfg(feature = "gc-memory-telemetry")]
#[test]
fn satb_inventory_counts_aligned_control_chunks_partials_and_sidecar() {
    let mut heap = TaggedHeap::new();
    let handoff = heap.satb_handoff.clone();
    for ordinal in 0..satb::SATB_CHUNK_WORDS + 1 {
        heap.current_mutator_satb_mut()
            .satb
            .push(MarkWord::of(TaggedValue::fixnum(ordinal as i64)), &handoff);
    }
    let inventory = memory_inventory::snapshot(&heap, false);
    let find = |name| {
        inventory
            .buffers
            .iter()
            .find(|buffer| buffer.name == name)
            .unwrap()
    };
    assert_eq!(find("satb-handoff-control").capacity_bytes, 256);
    assert_eq!(find("satb-chunk-descriptors").elements, 1);
    assert_eq!(find("satb-chunk-payloads").elements, satb::SATB_CHUNK_WORDS);
    assert_eq!(find("mutator-satb-partial").elements, 1);
    assert_eq!(
        find("mutator-satb-partial").capacity_bytes,
        satb::SATB_CHUNK_WORDS * size_of::<MarkWord>()
    );
    assert_eq!(
        find("mutator-satb-sidecar").capacity_bytes,
        heap._satb_layout_reserve.allocation_bytes()
    );
    assert!(find("mutator-satb-sidecar").capacity_bytes > 0);
}

#[test]
fn satb_test_owner_switch_restores_hot_cold_state_and_tls_after_panic() {
    let mut heap = TaggedHeap::new();
    set_tagged_heap(&mut heap);
    heap.current_mutator_gc_mut().bytes_since_gc = 11;
    heap.current_mutator_satb_mut()
        .satb_snapshotted_owners
        .insert(23);
    // SAFETY: this thread owns the complete fixture; switching performs only
    // owner-local Rust state writes, with no Lisp allocation or safepoint.
    let mut world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&mut heap) };
    let id = world.register_mutator_gc_for_test();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), _> = world.with_mutator_gc_for_test(id, |heap| {
            assert_eq!(heap.current_mutator_gc().bytes_since_gc, 0);
            assert!(
                heap.current_mutator_satb()
                    .satb_snapshotted_owners
                    .is_empty()
            );
            heap.current_mutator_gc_mut().bytes_since_gc = 31;
            heap.current_mutator_satb_mut()
                .satb_snapshotted_owners
                .insert(47);
            panic!("unwind from selected artificial mutator");
        });
    }));
    assert!(result.is_err());
    assert_eq!(world.heap().current_mutator_gc().bytes_since_gc, 11);
    assert!(
        world
            .heap()
            .current_mutator_satb()
            .satb_snapshotted_owners
            .contains(&23)
    );
    assert!(
        !world
            .heap()
            .current_mutator_satb()
            .satb_snapshotted_owners
            .contains(&47)
    );
    let selected = world.mutator_gc_for_test(id);
    assert_eq!(selected.gc.bytes_since_gc, 31);
    assert!(selected.satb.satb_snapshotted_owners.contains(&47));
    assert!(tagged_heap_is_current(world.heap()));
}
