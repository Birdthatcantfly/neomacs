use super::*;

/// MAIN390's old byte-tail offsets remain an explicit regression contract.
/// This is test evidence, not hardcoded generated-code offsets: native code
/// still obtains its two offsets through the owning layout constants above.
#[test]
fn compiled_exclusion_bytes_preserve_main390_heap_tail_layout() {
    crate::test_utils::init_test_tracing();
    let old_offsets = [
        std::mem::offset_of!(TaggedHeap, gc_threshold_overridden),
        std::mem::offset_of!(TaggedHeap, forced_termination_pending),
        std::mem::offset_of!(TaggedHeap, write_tracking_mode),
        std::mem::offset_of!(TaggedHeap, first_cycle_concurrent),
        std::mem::offset_of!(TaggedHeap, image_premarked),
        std::mem::offset_of!(TaggedHeap, partition_dump),
        std::mem::offset_of!(TaggedHeap, dump_blackened),
        std::mem::offset_of!(TaggedHeap, bootstrap_collected),
        std::mem::offset_of!(TaggedHeap, mark_parity),
        std::mem::offset_of!(TaggedHeap, mark_in_progress),
        std::mem::offset_of!(TaggedHeap, concurrent_mark_running),
        std::mem::offset_of!(TaggedHeap, sweep_in_progress),
        std::mem::offset_of!(TaggedHeap, vec_scan),
    ];
    let test_only_bytes = size_of::<usize>();
    let expected_main_offsets = [
        4512, 4513, 4514, 4515, 4516, 4517, 4518, 4519, 4520, 4521, 4522, 4523, 4524,
    ];
    assert_eq!(
        old_offsets.map(|offset| offset - test_only_bytes),
        expected_main_offsets,
        "account only for the existing test-only mapped_veclike_traces usize"
    );
    assert_eq!(HEAP_JIT_EXCLUSION_LO - test_only_bytes, 4525);
    assert_eq!(HEAP_JIT_EXCLUSION_LEN - test_only_bytes, 4533);
    assert_eq!(HEAP_JIT_CONS_PROOFS - test_only_bytes, 4565);
}

#[test]
fn compiled_cons_proof_availability_tracks_published_protocol_transitions() {
    crate::test_utils::init_test_tracing();
    let mut heap = TaggedHeap::new();
    heap.generational.enabled = false;
    assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Absent);
    let observed = [BarrierWindow::span(128, 1024), BarrierWindow::NONE];
    let exact = CompiledObservationGate {
        observed,
        single_cons: 512,
        ..CompiledObservationGate::NONE
    };
    let gap = CompiledObservationGate {
        observed,
        excluded: Some(BarrierWindow::span(256, 512)),
        ..CompiledObservationGate::NONE
    };

    for gate in [
        exact,
        gap,
        CompiledObservationGate {
            single_cons: 512,
            ..gap
        },
    ] {
        heap.set_compiled_barrier_window(BarrierWindow::NONE, gate);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Present);
        // Tracking suppresses the proofs even when the prepared gate retains
        // nonzero words. Leaving tracking republishes the available proof.
        heap.set_compiled_barrier_window(BarrierWindow::ALL, gate);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Absent);
        heap.set_compiled_barrier_window(BarrierWindow::NONE, gate);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Present);
        heap.generational.enabled = true;
        heap.set_compiled_barrier_window(BarrierWindow::NONE, gate);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Absent);
        heap.generational.enabled = false;
        heap.set_compiled_barrier_window(BarrierWindow::NONE, gate);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Present);
        // Closing the observation epoch clears stale exact/gap availability.
        heap.set_compiled_barrier_window(BarrierWindow::NONE, CompiledObservationGate::NONE);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Absent);
    }
    // An observed hull, including an independent slot hull, supplies no cons
    // negative proof by itself. The availability byte must remain absent.
    for gate in [
        CompiledObservationGate {
            observed,
            ..CompiledObservationGate::NONE
        },
        CompiledObservationGate {
            slots: observed,
            ..CompiledObservationGate::NONE
        },
        CompiledObservationGate {
            single_cons: 512,
            ..CompiledObservationGate::NONE
        },
        CompiledObservationGate {
            excluded: gap.excluded,
            ..CompiledObservationGate::NONE
        },
    ] {
        heap.set_compiled_barrier_window(BarrierWindow::NONE, gate);
        assert_eq!(heap.compiled_cons_proofs.get(), CompiledConsProofs::Absent);
    }
}
