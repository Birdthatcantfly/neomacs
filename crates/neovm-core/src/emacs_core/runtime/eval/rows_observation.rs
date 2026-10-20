//! Partial physical-owner observations for the private numeric Rows producer.
//! No public API here grants geometry, coverage or cached-query authority.
use super::Context;
use crate::tagged::{
    collection_reads::CollectionReads,
    gc::{self, HeapIdentity, RowsOwnedDomain},
};
use crate::window::FrameId;
use std::{marker::PhantomData, ops::Range};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RowsObservationRefusal {
    #[error("the Context is not the installed owning mutator")]
    InactiveContext,
    #[error("GEN0 does not provide the required partial/error collector refusal")]
    GenerationalDisabled,
    #[error("a runtime collector driver is active or unwound")]
    DriverActive,
    #[error("the collector or owner transport is not idle")]
    CollectorBusy,
    #[error("the logical Context, heap, face generation or dump authority changed")]
    RuntimeChanged,
    #[error("a collection completed after observation began")]
    CollectionChanged,
    #[error("a native display host can enter external code")]
    DisplayHostInstalled,
    #[error("only an explicit graphical window-system frame is supported")]
    TtyOrUnknownFrame,
    #[error("face normalization is not already warm")]
    ColdFaces,
    #[error("the existing chunk map is disabled")]
    ChunkMapDisabled,
    #[error("a dependency is foreign, new-domain, boxed or unsupported")]
    UnknownOwner,
}

/// Inert pre-read owner-domain authority, never object/source references.
#[derive(Debug)]
pub struct RowsObservationEpoch {
    heap: HeapIdentity,
    collections: usize,
    context: u64,
    frame: FrameId,
    faces: u64,
    owned: RowsOwnedDomain,
    dump: Option<(usize, Range<usize>)>,
    _local: PhantomData<*const ()>,
}

/// Conditional numeric membership and mutation metadata only.
///
/// This public type does NOT bind the supplied read set to a capture interval,
/// retain dependency owners, or grant permission to read any saved pointer.
/// A private consumer must establish that this epoch preceded the WHOLE actual
/// capture before deriving physical-origin/lifetime authority from membership.
#[derive(Debug)]
pub struct RowsDependencyMetadata<'reads> {
    epoch: RowsObservationEpoch,
    reads: &'reads CollectionReads,
    _local: PhantomData<*const ()>,
}
static_assertions::assert_not_impl_any!(RowsObservationEpoch: Send, Sync);
static_assertions::assert_not_impl_any!(RowsDependencyMetadata<'static>: Send, Sync);

impl Context {
    // Closed native checks: no normalization, tracing, service, Lisp or GC.
    fn rows_environment_current(&self, frame: FrameId) -> Result<(), RowsObservationRefusal> {
        if !gc::tagged_heap_is_current(&self.tagged_heap) {
            return Err(RowsObservationRefusal::InactiveContext);
        }
        if !self.tagged_heap.generational_enabled() {
            return Err(RowsObservationRefusal::GenerationalDisabled);
        }
        if self.gc_driver_active {
            return Err(RowsObservationRefusal::DriverActive);
        }
        if !self.tagged_heap.rows_observation_idle() {
            return Err(RowsObservationRefusal::CollectorBusy);
        }
        if self.display_host.is_some() {
            return Err(RowsObservationRefusal::DisplayHostInstalled);
        }
        let owner = self
            .frames
            .get(frame)
            .ok_or(RowsObservationRefusal::TtyOrUnknownFrame)?;
        // Strict first scope: even initial/dummy terminal frames refuse. This
        // is stronger than frame_is_tty_face_target's real-termcap predicate.
        if owner.effective_window_system().is_none() {
            return Err(RowsObservationRefusal::TtyOrUnknownFrame);
        }
        let warm = if crate::face::realization::enabled() {
            owner
                .face_realization
                .table(self.face_change_count)
                .is_some()
        } else {
            (*self.materialized_face_table_source) == Some((frame, self.face_change_count))
        };
        if !warm {
            return Err(RowsObservationRefusal::ColdFaces);
        }
        Ok(())
    }

    /// Before the WHOLE capture_normalized, including normalization's reads.
    /// Copies native class counts and the actual retained pdump's exact bounds.
    /// It holds no BufferText Ref, Lisp Value, object/header borrow or GC root.
    pub fn begin_static_rows_observation(
        &self,
        frame: FrameId,
    ) -> Result<RowsObservationEpoch, RowsObservationRefusal> {
        self.rows_environment_current(frame)?;
        let owned = self
            .tagged_heap
            .capture_rows_owned_domain()
            .ok_or(RowsObservationRefusal::ChunkMapDisabled)?;
        let dump = self.pdump_image.map(|image| {
            (
                image as *const _ as usize,
                image.rows_mapping_address_range(),
            )
        });
        Ok(RowsObservationEpoch {
            heap: self.tagged_heap.heap_identity(),
            collections: self.tagged_heap.gc_collections(),
            context: self.context_instance_id,
            frame,
            faces: self.face_change_count,
            owned,
            dump,
            _local: PhantomData,
        })
    }

    /// Must precede numeric-key classification, source lease and header reads.
    pub fn static_rows_observation_current(
        &self,
        epoch: &RowsObservationEpoch,
    ) -> Result<(), RowsObservationRefusal> {
        // Compare current native owners without consulting saved pointer keys.
        if !gc::tagged_heap_is_current(&self.tagged_heap) {
            return Err(RowsObservationRefusal::InactiveContext);
        }
        if self.tagged_heap.heap_identity() != epoch.heap
            || self.context_instance_id != epoch.context
        {
            return Err(RowsObservationRefusal::RuntimeChanged);
        }
        if !self.tagged_heap.generational_enabled() {
            return Err(RowsObservationRefusal::GenerationalDisabled);
        }
        if self.gc_driver_active {
            return Err(RowsObservationRefusal::DriverActive);
        }
        if !self.tagged_heap.rows_observation_idle() {
            return Err(RowsObservationRefusal::CollectorBusy);
        }
        if self.tagged_heap.gc_collections() != epoch.collections {
            return Err(RowsObservationRefusal::CollectionChanged);
        }
        if self.face_change_count != epoch.faces
            || self.pdump_image.map(|image| image as *const _ as usize)
                != epoch.dump.as_ref().map(|(identity, _)| *identity)
        {
            return Err(RowsObservationRefusal::RuntimeChanged);
        }
        self.rows_environment_current(epoch.frame)
    }

    /// Safe numeric domain matching only, not lifetime or query admission.
    /// This does not establish that `epoch` preceded these particular `reads`.
    /// The private layout consumer owns that ordering proof at its sole actual
    /// canonical-capture call site. Old/arbitrary read sets cannot derive a
    /// lifetime theorem merely by matching this conditional metadata.
    pub fn finish_static_rows_observation<'reads>(
        &self,
        epoch: RowsObservationEpoch,
        reads: &'reads CollectionReads,
    ) -> Result<RowsDependencyMetadata<'reads>, RowsObservationRefusal> {
        self.static_rows_observation_current(&epoch)?;
        let known = reads.all_dependency_bits(|bits| {
            use crate::tagged::value::{TAG_CONS, TAG_MASK, TAG_STRING, TAG_VECLIKE};
            if !matches!(bits & TAG_MASK, TAG_CONS | TAG_STRING | TAG_VECLIKE) {
                return false;
            }
            let address = bits & !TAG_MASK;
            address != 0
                && (self
                    .tagged_heap
                    .rows_dependency_in_start_domain(&epoch.owned, bits)
                    || epoch
                        .dump
                        .as_ref()
                        .is_some_and(|(_, range)| range.contains(&address)))
        });
        if !known {
            return Err(RowsObservationRefusal::UnknownOwner);
        }
        Ok(RowsDependencyMetadata {
            epoch,
            reads,
            _local: PhantomData,
        })
    }
}

impl RowsDependencyMetadata<'_> {
    /// Recheck existing native owners/collector and numeric mutation metadata.
    /// This performs no saved-key header dereference and grants no pointer
    /// permission after return. It cannot establish the read-set interval or
    /// prevent a later callback from collecting. Only the private, ordered,
    /// inert Layout consumer derives a physical-owner proof from these facts.
    pub fn current_and_observe(&self, context: &Context) -> bool {
        context.static_rows_observation_current(&self.epoch).is_ok()
            && self.reads.unchanged_and_observe()
    }
}

#[cfg(test)]
#[path = "tests/rows_observation_test.rs"]
mod tests;
