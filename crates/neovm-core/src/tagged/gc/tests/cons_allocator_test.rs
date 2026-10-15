//! Focused tests of the shared allocator, without making TaggedHeap Send.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

fn free_block(allocator: &mut ConsAllocator, cells: usize) -> ConsBlock {
    let mut block = ConsBlock::new();
    block.reserve_tail(cells);
    allocator.register(&block);
    let mut head = std::ptr::null_mut();
    let live = block.sweep(&mut head);
    assert_eq!(live, 0);
    // SAFETY: freshly owned block remains alive for each test, its sweep
    // produced an exact list and there are no mutators or outstanding regions.
    unsafe { allocator.publish(&block, head, live) };
    block
}

#[test]
fn cons_allocator_detaches_nonlast_block_and_withholds_returned_cells() {
    let mut allocator = ConsAllocator::default();
    let blocks: Vec<_> = (0..3).map(|_| free_block(&mut allocator, 32)).collect();
    let first = ConsBlockBase::of(&blocks[0]);
    let last = ConsBlockBase::of(&blocks[2]);
    // SAFETY: no worker or region exists in this exclusively owned fixture.
    unsafe { allocator.stopped_world() }.withhold([first]);
    allocator.verify();
    assert!(!allocator.is_ready(first));
    assert!(allocator.is_ready(last));
    // SAFETY: the withheld block remains owned; withholding emptied its old
    // list, so this range is exclusive and currently absent from every list.
    unsafe { allocator.return_unused(blocks[0].cells_ptr(), 8) };
    assert!(!allocator.is_ready(first));
    allocator.verify();
    // SAFETY: the first block is withheld, so the remaining two ready lists
    // are stable, valid, and exclude the returned region above.
    let (run, count) = unsafe { allocator.reserve_run(32) }.unwrap();
    assert_ne!(ConsBlock::block_base_for_ptr(run), first.addr());
    assert_eq!(count, 32);
    allocator.unregister(first);
    allocator.verify();
    for block in &blocks[1..] {
        allocator.unregister(ConsBlockBase::of(block));
    }
}

#[test]
fn cons_allocator_three_mutators_share_free_blocks_across_rebuilds() {
    enum WorkerCommand {
        Allocate(usize),
    }
    const MUTATORS: usize = 3;
    const CYCLES: usize = 128;
    const CARVES: usize = 32;
    let mut allocator = ConsAllocator::default();
    let mut blocks: Vec<_> = (0..3).map(|_| free_block(&mut allocator, 256)).collect();
    let bases: Vec<_> = blocks.iter().map(ConsBlock::base_addr).collect();
    let active: Vec<_> = (0..3 * 256).map(|_| AtomicBool::new(false)).collect();
    // SAFETY: the fixture has no workers, regions or marker yet.
    unsafe { allocator.stopped_world() }.admit_threads();
    {
        let allocator = allocator.shared();
        std::thread::scope(|scope| {
            // These senders belong INSIDE the scope closure: coordinator unwind
            // drops them before scoped thread joining, waking every idle worker.
            let mut commands = Vec::new();
            let (result_tx, results) = mpsc::channel();
            for worker in 0..MUTATORS {
                let allocator = &allocator;
                let active = &active;
                let bases = &bases;
                let (command_tx, command_rx) = mpsc::channel();
                commands.push(command_tx);
                let result_tx = result_tx.clone();
                scope.spawn(move || {
                    while let Ok(WorkerCommand::Allocate(cycle)) = command_rx.recv() {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            for carve in 0..CARVES {
                                let want = 1 + (worker + cycle + carve) % 17;
                                // SAFETY: parent retains blocks through join and
                                // collects only after every worker has replied.
                                let (run, count) = unsafe { allocator.reserve_run(want) }.unwrap();
                                let base = ConsBlock::block_base_for_ptr(run);
                                let block_index = bases.iter().position(|&b| b == base).unwrap();
                                for index in 0..count {
                                    // SAFETY: this reserved run is exclusively
                                    // owned and remains in its registered block.
                                    let cell = unsafe { run.add(index) };
                                    let slot = block_index * 256 + ConsBlock::index_of_ptr(cell);
                                    assert!(
                                        !active[slot].swap(true, Ordering::SeqCst),
                                        "overlapping runs"
                                    );
                                    // SAFETY: initialize privately owned cells.
                                    // No global test lock serializes cell writes.
                                    unsafe {
                                        (*cell).car = TaggedValue::T;
                                        (*cell).set_cdr(TaggedValue::NIL);
                                    }
                                }
                                std::thread::yield_now();
                                for index in 0..count {
                                    // SAFETY: the entire run is still owned.
                                    let cell = unsafe { run.add(index) };
                                    let slot = block_index * 256 + ConsBlock::index_of_ptr(cell);
                                    assert!(active[slot].swap(false, Ordering::SeqCst));
                                }
                                // SAFETY: unreachable region is exclusive, and
                                // coordinator has not started any sweep.
                                unsafe { allocator.return_unused(run, count) };
                            }
                        }));
                        let failed = result.is_err();
                        if result_tx.send((worker, cycle, result)).is_err() || failed {
                            break;
                        }
                    }
                });
            }
            drop(result_tx);
            for cycle in 0..CYCLES {
                for command in &commands {
                    command.send(WorkerCommand::Allocate(cycle)).unwrap();
                }
                let mut replied = std::collections::HashSet::new();
                for _ in 0..MUTATORS {
                    let (worker, returned_cycle, result) = results.recv().unwrap();
                    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                    assert_eq!(returned_cycle, cycle);
                    assert!(replied.insert(worker));
                }
                // Every worker is now blocked on its private command receiver;
                // successful replies are the stop-all/closed-region proof.
                assert!(active.iter().all(|slot| !slot.load(Ordering::SeqCst)));
                assert_eq!(allocator.free_count(), 3 * 256);
                allocator.verify();
                // SAFETY: every scoped worker is parked with its regions closed.
                unsafe { allocator.stopped_world() }.withhold(blocks.iter().map(ConsBlockBase::of));
                assert!(allocator.free_heads().is_empty());
                // SAFETY: every worker replied with all regions returned.
                assert!(unsafe { allocator.reserve_run(1) }.is_none());
                for block in &mut blocks {
                    block.clear_marks();
                    let mut head = std::ptr::null_mut();
                    let live = block.sweep(&mut head);
                    assert_eq!(live, 0);
                    // SAFETY: workers remain parked; this exact rebuilt list
                    // names storage retained by the coordinator until scope join.
                    unsafe { allocator.publish(block, head, live) };
                }
                allocator.verify();
            }
            // Normal completion and unwinding both disconnect idle receivers.
            drop(commands);
        });
    }
    for block in &blocks {
        allocator.unregister(ConsBlockBase::of(block));
    }
}

#[test]
fn cons_allocator_poison_prevents_collector_panic_containment() {
    let mut heap = TaggedHeap::new();
    assert!(!heap.gc_locks_poisoned());
    // SAFETY: no workers, regions or marker exist in this fresh fixture.
    unsafe { heap.cons_allocator.stopped_world() }.admit_threads();
    let shared = heap.cons_allocator.shared();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = shared.state.lock().unwrap();
        panic!("inject allocator protocol failure");
    }));
    assert!(result.is_err());
    assert!(heap.gc_locks_poisoned());
}

#[test]
fn cons_allocator_dense_slots_preserve_fragmented_run_and_return_order() {
    let mut allocator = ConsAllocator::default();
    let mut blocks = [free_block(&mut allocator, 8), free_block(&mut allocator, 7)];
    let bases = blocks.each_ref().map(ConsBlockBase::of);
    // SAFETY: no workers or outstanding reservations exist in this fixture.
    unsafe { allocator.stopped_world() }.withhold(bases);
    for (block, live_indices) in blocks.iter_mut().zip([&[2, 6][..], &[1, 5][..]]) {
        for &index in live_indices {
            ConsBlock::mark_run_at(block.base_addr(), index, 1, true);
        }
        let mut head = std::ptr::null_mut();
        let live = block.sweep(&mut head);
        // SAFETY: a stopped fixture rebuilt this exact list; blocks remain owned.
        unsafe { allocator.publish(block, head, live) };
    }
    // The newest published block's isolated first cell is a short run. Giving
    // it back must restore that same first run, rather than reorder blocks.
    // SAFETY: all free nodes remain in these registered, owned blocks.
    let (first, count) = unsafe { allocator.reserve_run(3) }.unwrap();
    assert_eq!(ConsBlockBase::of_cell(first), bases[1]);
    assert_eq!(ConsBlock::index_of_ptr(first), 0);
    assert_eq!(count, 1);
    // SAFETY: this uninitialized, unmarked run remains exclusively ours.
    unsafe { allocator.return_unused(first, count) };
    for (base, index, expected_count) in [
        (bases[1], 0, 1),
        (bases[1], 2, 3),
        (bases[1], 6, 1),
        (bases[0], 0, 2),
        (bases[0], 3, 3),
        (bases[0], 7, 1),
    ] {
        // SAFETY: fixture retains every block and owns every preceding grant.
        let (run, count) = unsafe { allocator.reserve_run(3) }.unwrap();
        assert_eq!(ConsBlockBase::of_cell(run), base);
        assert_eq!(ConsBlock::index_of_ptr(run), index);
        assert_eq!(count, expected_count);
        allocator.verify();
    }
    // SAFETY: storage is still retained; all free runs were reserved above.
    assert!(unsafe { allocator.reserve_run(1) }.is_none());
    for base in bases {
        allocator.unregister(base);
    }
    allocator.verify();
}

#[test]
fn cons_allocator_dense_slots_reuse_identity_after_unlink_and_withhold() {
    let mut allocator = ConsAllocator::default();
    let blocks = [
        free_block(&mut allocator, 9),
        free_block(&mut allocator, 5),
        free_block(&mut allocator, 7),
    ];
    let bases = blocks.each_ref().map(ConsBlockBase::of);
    let removed_slot = allocator.with_state(|state, _| state.slot(bases[1]));
    allocator.unregister(bases[1]);
    allocator.unregister(bases[1]); // Already absent: never enqueue a vacant slot twice.
    assert!(!allocator.is_ready(bases[1]));
    allocator.verify();
    // Keep the removed block owned, ensuring a different base takes its slot.
    let mut replacement = free_block(&mut allocator, 11);
    let replacement_base = ConsBlockBase::of(&replacement);
    assert_ne!(replacement_base, bases[1]);
    assert_eq!(
        allocator.with_state(|state, _| state.slot(replacement_base)),
        removed_slot
    );
    assert_eq!(allocator.with_state(|state, _| state.slots.len()), 3);
    allocator.verify();
    // SAFETY: no active workers/regions; nonlast removals exercise the moved
    // ready entry's direct metadata backpointer before the slot is reused.
    unsafe { allocator.stopped_world() }.withhold([bases[0], replacement_base]);
    // SAFETY: withholding removed every replacement node; this is an exclusive
    // returned range in retained storage and must remain unavailable.
    unsafe { allocator.return_unused(replacement.cells_ptr(), 3) };
    assert!(!allocator.is_ready(replacement_base));
    // SAFETY: only the final original block remains available.
    let (run, count) = unsafe { allocator.reserve_run(64) }.unwrap();
    assert_eq!(ConsBlockBase::of_cell(run), bases[2]);
    assert_eq!(count, 7);
    // SAFETY: all available nodes were granted; the replacement is withheld.
    assert!(unsafe { allocator.reserve_run(1) }.is_none());
    for _ in 0..64 {
        allocator.unregister(replacement_base);
        allocator.register(&replacement);
        allocator.with_state(|state, _| {
            assert_eq!(state.slot(replacement_base), removed_slot);
            assert_eq!(
                state.slots.len(),
                3,
                "slot storage grew with registration churn"
            );
            let entry = state.block(removed_slot);
            assert_eq!(entry.head, 0);
            assert_eq!(entry.count, 0);
            assert_eq!(entry.available_slot, None);
            assert_eq!(entry.availability, BlockAvailability::Ready);
        });
        let mut head = std::ptr::null_mut();
        let live = replacement.sweep(&mut head);
        assert_eq!(live, 0);
        // SAFETY: no replacement grant remains active; its exact list was rebuilt.
        unsafe { allocator.publish(&replacement, head, live) };
        allocator.verify();
        // SAFETY: the replacement is the only available block and remains owned.
        let (run, count) = unsafe { allocator.reserve_run(64) }.unwrap();
        assert_eq!(ConsBlockBase::of_cell(run), replacement_base);
        assert_eq!(count, 11);
    }
    allocator.unregister(replacement_base);
    allocator.unregister(bases[0]);
    allocator.unregister(bases[2]);
    allocator.verify();
}

#[test]
fn sole_allocator_admission_preserves_free_run_inventory() {
    let mut allocator = ConsAllocator::default();
    let block = free_block(&mut allocator, 8);
    let base = ConsBlockBase::of(&block);
    assert!(!allocator.threads_admitted());
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = allocator.shared();
    }));
    assert!(refused.is_err());
    assert!(!allocator.threads_admitted());
    // SAFETY: the fixture retains its registered block and consumes this
    // complete grant, leaving no open reservation at the admission boundary.
    let (first, count) = unsafe { allocator.reserve_run(3) }.unwrap();
    assert_eq!(count, 3);
    for index in 0..count {
        // SAFETY: all cells in the private run are initialized by its owner.
        unsafe {
            (*first.add(index)).car = TaggedValue::T;
            (*first.add(index)).set_cdr(TaggedValue::NIL);
        }
    }
    let slot = allocator.with_state(|state, _| state.slot(base));
    assert_eq!(allocator.free_count(), 5);
    // SAFETY: a stopped fixture owns every cell and has no marker or worker.
    unsafe { allocator.stopped_world() }.admit_threads();
    assert!(allocator.threads_admitted());
    {
        let shared = allocator.shared();
        assert_eq!(shared.free_count(), 5);
        shared.with_state(|state, _| assert_eq!(state.slot(base), slot));
        // SAFETY: all registered storage is still retained.
        let (rest, count) = unsafe { shared.reserve_run(8) }.unwrap();
        assert_eq!(ConsBlock::index_of_ptr(rest), 3);
        assert_eq!(count, 5);
        // SAFETY: this entire second grant remains unhanded and unmarked.
        unsafe { shared.return_unused(rest, count) };
        shared.verify();
    }
    assert!(allocator.threads_admitted());
    allocator.unregister(base);
}

#[test]
fn sole_allocator_stop_all_unwind_is_poisoned() {
    let mut heap = TaggedHeap::new();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the fresh fixture has no worker, region or marker.
        let mut stopped = unsafe { heap.cons_allocator.stopped_world() };
        stopped.state.blocks_with_free.reserve(8);
        panic!("inject sole collector metadata failure");
    }));
    assert!(failure.is_err());
    assert!(heap.gc_locks_poisoned());
    assert!(!heap.cons_allocator.threads_admitted());
}
