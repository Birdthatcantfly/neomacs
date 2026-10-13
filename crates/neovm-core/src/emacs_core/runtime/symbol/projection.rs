//! Thread-projected cells (Phase 0 step 2): reveal and conceal.
//!
//! A projected cell is `Forwarded` over a [`ThreadProjectionDescriptor`]
//! (see `forward::thread_projection`). For a single owner, a projected
//! variable must behave exactly like the variable it wraps. The slow paths
//! that meet a projected cell therefore REVEAL it -- give the cell back its
//! logical arm and word for the length of the operation -- run the existing
//! operation on that unprojected representation, and CONCEAL it again,
//! recording whatever the operation left as the new canonical storage.
//!
//! Reveal and conceal are cell writes like any other ([`CellWrite`]): a
//! running mark sees the old pair or the new one, and a plain word they
//! overwrite is noted first. While revealed, the variable IS its logical
//! cell, so Lisp an operation runs (a watcher) observes exactly the
//! unprojected variable, and a nested operation on the same symbol finds no
//! projection and runs as it always has. Compiled code baked against either
//! representation re-tests the cell's redirect or word on every access, so
//! nothing it caches outlives a reveal or a conceal.
//!
//! Dynamic bindings never reveal: they live in the binding owner's slab
//! (`eval::owner_bindings`), and no Lisp runs while a cell is revealed.

use super::cell::{ArmMut, RevealedCell};
use super::{Obarray, SymId};
use crate::emacs_core::forward::{
    BuiltinForwarder, CanonicalArm, CanonicalBlv, ThreadProjectionDescriptor,
};

/// A projected cell an operation has revealed, to be concealed again with
/// [`Obarray::conceal_projection`]. Neither `Copy` nor `Clone`: one reveal,
/// one conceal.
#[must_use = "a revealed projection must be concealed again"]
#[derive(Debug)]
pub(crate) struct RevealedProjection {
    id: SymId,
    projection: &'static ThreadProjectionDescriptor,
}

static_assertions::assert_not_impl_any!(RevealedProjection: Copy, Clone, Send, Sync);

/// What concealing a revealed cell found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConcealOutcome {
    /// The cell is projected again over its (possibly changed) logical arm.
    Concealed,
    /// The operation made the variable an alias (`defvaralias`): an alias
    /// stays an alias and resolves to its base, so the projection retires.
    RetiredToAlias,
}

impl Obarray {
    /// The thread projection ID's cell heads, if it is projected.
    #[inline]
    pub(crate) fn thread_projection_of(
        &self,
        id: SymId,
    ) -> Option<&'static ThreadProjectionDescriptor> {
        self.slot(id)?.forwarded_descriptor()?.thread_projection()
    }

    /// Give ID's projected cell its logical arm, word and host-projection
    /// bit back. `None`, changing nothing, unless ID is projected.
    pub(crate) fn reveal_projection(&mut self, id: SymId) -> Option<RevealedProjection> {
        let projection = self.thread_projection_of(id)?;
        let logical = match projection.arm() {
            CanonicalArm::Plain => RevealedCell::Plain(projection.plain_value()),
            CanonicalArm::Localized(blv) => RevealedCell::Localized(blv.blv()),
            CanonicalArm::Forwarded(builtin) => RevealedCell::Forwarded(builtin.descriptor()),
        };
        {
            let mut write = self.cell_write_ensure(id);
            let ArmMut::Forwarded(forwarded) = write.arm() else {
                unreachable!("a projected cell is forwarded");
            };
            forwarded.reveal_projection(logical);
        }
        self.ensure_slot(id)
            .set_runtime_projected(projection.host_projected());
        Some(RevealedProjection { id, projection })
    }

    /// Conceal a revealed cell again over its projection, recording the
    /// logical arm and word the operation left.
    pub(crate) fn conceal_projection(&mut self, revealed: RevealedProjection) -> ConcealOutcome {
        let RevealedProjection { id, projection } = revealed;
        {
            let mut write = self.cell_write_ensure(id);
            match write.arm() {
                ArmMut::Plain(plain) => {
                    projection.set_plain_value(plain.value());
                    projection.set_arm(CanonicalArm::Plain);
                    plain.conceal_projection(projection);
                }
                ArmMut::Localized(local) => {
                    projection.set_arm(CanonicalArm::Localized(CanonicalBlv::new(local.blv())));
                    local.conceal_projection(projection);
                }
                ArmMut::Forwarded(forwarded) => {
                    let fwd = forwarded.descriptor();
                    // A nested operation may already have concealed it.
                    if !std::ptr::eq(fwd, projection.header()) {
                        let builtin = BuiltinForwarder::new(fwd)
                            .expect("a revealed cell never forwards to another projection");
                        projection.set_arm(CanonicalArm::Forwarded(builtin));
                        forwarded.conceal_projection(projection);
                    }
                }
                ArmMut::Alias(_) => return ConcealOutcome::RetiredToAlias,
            }
        }
        self.mark_runtime_projected_id(id);
        ConcealOutcome::Concealed
    }

    /// Project `id`'s variable to owner-local bindings: its cell becomes
    /// `Forwarded` over a descriptor TABLE issues, whose canonical storage
    /// keeps the cell's logical arm and word, and its host-projection bit
    /// is set so every checked writer refuses it, as stopped admission will
    /// require. `None`, changing nothing, for an alias (it resolves to its
    /// base) or a variable already projected.
    ///
    /// Test-only: no production path converts a symbol yet.
    #[cfg(test)]
    pub(crate) fn project_thread_variable_for_test(
        &mut self,
        id: SymId,
        table: &mut crate::emacs_core::forward::ThreadProjectionTable,
    ) -> Option<&'static ThreadProjectionDescriptor> {
        use crate::emacs_core::value::Value;
        let host_projected = self.ensure_slot(id).flags().runtime_projected();
        let projection = {
            let mut write = self.cell_write_ensure(id);
            match write.arm() {
                ArmMut::Plain(plain) => {
                    let projection =
                        table.project_for_test(CanonicalArm::Plain, plain.value(), host_projected);
                    plain.conceal_projection(projection);
                    projection
                }
                ArmMut::Localized(local) => {
                    let projection = table.project_for_test(
                        CanonicalArm::Localized(CanonicalBlv::new(local.blv())),
                        Value::UNBOUND,
                        host_projected,
                    );
                    local.conceal_projection(projection);
                    projection
                }
                ArmMut::Forwarded(forwarded) => {
                    let builtin = BuiltinForwarder::new(forwarded.descriptor())?;
                    let projection = table.project_for_test(
                        CanonicalArm::Forwarded(builtin),
                        Value::UNBOUND,
                        host_projected,
                    );
                    forwarded.conceal_projection(projection);
                    projection
                }
                ArmMut::Alias(_) => return None,
            }
        };
        self.mark_runtime_projected_id(id);
        self.register_value_fwd(projection.plain_root());
        Some(projection)
    }
}

impl Obarray {
    /// GNU `find_symbol_value` of a projected variable whose logical arm is
    /// `Localized` BLV, in BUFFER (the record's default without one): the
    /// read [`Self::read_localized_in_buffer`] makes, on the record the
    /// projection names rather than the one a cell would.
    pub(crate) fn read_projected_localized(
        &self,
        id: SymId,
        blv: CanonicalBlv,
        buffer: Option<&crate::buffer::Buffer>,
    ) -> crate::emacs_core::value::Value {
        use crate::emacs_core::value::Value;
        let blv_ptr = blv.blv().as_ptr();
        match buffer {
            // SAFETY: a projection's localized arm names a record this
            // obarray's BLV pool owns (it was the cell's record when the
            // variable was projected or revealed), and no reference to it
            // is held across the call.
            Some(buffer) => unsafe {
                Self::read_blv_with_lookup(blv_ptr, Value::make_buffer(buffer.id), || {
                    buffer.local_variable_binding_cell(id).unwrap_or(Value::NIL)
                })
            },
            // SAFETY: as above; the default cell is read without a write.
            None => unsafe { (*blv_ptr).defcell.cons_cdr() },
        }
    }
}

impl Obarray {
    /// Run OP with ID's projected cell revealed, then conceal it again: the
    /// storage-level form of `Context::with_revealed_projection`, for writers
    /// below the evaluator. ID must be projected; OP sees the unprojected
    /// variable.
    pub(crate) fn with_revealed_projection<R>(
        &mut self,
        id: SymId,
        op: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let revealed = self.reveal_projection(id);
        let result = op(self);
        if let Some(revealed) = revealed {
            let _ = self.conceal_projection(revealed);
        }
        result
    }

    /// GNU `default_value` of a projected variable without buffer context:
    /// the plain word, the BLV default cell, or the builtin's own storage
    /// (a per-buffer builtin's installation default). `None` when void.
    #[cold]
    #[inline(never)]
    pub(crate) fn projected_default_value(
        &self,
        projection: &ThreadProjectionDescriptor,
    ) -> Option<crate::emacs_core::value::Value> {
        let value = match projection.arm() {
            CanonicalArm::Plain => projection.plain_value(),
            // SAFETY: a projection's localized arm names a record this
            // obarray's BLV pool owns; the default cell is only read.
            CanonicalArm::Localized(blv) => unsafe { (*blv.blv().as_ptr()).defcell.cons_cdr() },
            CanonicalArm::Forwarded(builtin) => {
                let fwd = builtin.descriptor();
                return fwd
                    .load()
                    .or_else(|| fwd.as_buffer_obj_fwd().map(|buf_fwd| buf_fwd.default));
            }
        };
        (!value.is_unbound()).then_some(value)
    }
}
