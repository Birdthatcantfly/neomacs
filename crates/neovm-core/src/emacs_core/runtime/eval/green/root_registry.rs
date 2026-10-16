//! Cold completion of World-owned parked-root obligations.

use crate::tagged::gc::QuiescentCollector;
use crate::tagged::transport::{RootBatchError, RootBatchFinishFailure, RootBatchLease};

/// The caller has restored the owner with no active mark, concurrent marker or
/// synchronous collection extent; a completed mark's cooperative sweep may
/// remain. Terminated-owner callers instead hold full collector quiescence.
/// SharedRoot retirement/registration on another host thread can briefly own
/// the same table lock. Retry that contention without dropping the lease.
pub(super) fn finish(mut publication: RootBatchLease) -> Result<(), RootBatchFinishFailure> {
    loop {
        match publication.try_finish() {
            Ok(_) => return Ok(()),
            Err(failure) if matches!(failure.error(), RootBatchError::Busy) => {
                (publication, _) = failure.into_parts();
                std::thread::yield_now();
            }
            Err(failure) => return Err(failure),
        }
    }
}

impl super::GreenRuntime {
    /// All owners have restored or terminated, either after teardown's joins
    /// or at exclusive World destruction. The borrowed drain proof prevents
    /// any collection until the final registration has explicitly finished.
    pub(in crate::emacs_core::eval) fn finish_terminated_roots(
        &self,
        collector: &QuiescentCollector<'_>,
    ) {
        let mut parked = self.parked.borrow_mut();
        for publication in parked.values() {
            assert_eq!(publication.heap_identity(), collector.heap_identity());
        }
        while let Some(key) = parked.keys().next().copied() {
            let publication = parked.remove(&key).expect("existing key");
            if let Err(failure) = finish(publication) {
                // An escaped/abandoned explicit reader is an invariant
                // violation here: safe readers borrow a live heap/lease.
                // Preserve registry ownership even during unwinding.
                let (publication, error) = failure.into_parts();
                parked.insert(key, publication);
                panic!("terminated Green root batch cannot finish: {error}");
            }
        }
    }
}

impl super::Context {
    /// Called only after main has restored and joined every worker.
    pub(super) fn finish_ended_parked_roots(&mut self) {
        if self.with_green_runtime(|runtime| runtime.parked.borrow().is_empty()) {
            return;
        }
        let heap: *mut crate::tagged::gc::TaggedHeap = &mut *self.tagged_heap;
        // SAFETY: main holds the Baton after all joins, with no callback or
        // collection extent active. The full registered census remains rooted
        // until mark and sweep finish and every lease has explicitly retired.
        let collector = unsafe {
            (*heap).drain_to_quiescent(|heap| {
                self.seed_registered_mutator_roots_world_stopped(heap);
            })
        }
        .expect("teardown drains its restored owner's collector");
        self.with_green_runtime(|runtime| runtime.finish_terminated_roots(&collector));
    }
}
