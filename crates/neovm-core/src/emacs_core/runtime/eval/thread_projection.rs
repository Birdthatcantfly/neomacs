//! Routing a thread-projected variable to its canonical storage (Phase 0
//! step 2, round 2).
//!
//! For a single owner a projected variable behaves exactly like the variable
//! it wraps. Every hot path refuses a projected cell (see
//! `forward::thread_projection`); the slow paths it then reaches recognise
//! it inside their existing per-buffer handling -- on a branch a real
//! per-buffer forwarder never takes, because its slot index is valid -- and
//! come here:
//! - reads with only `&self` compute the logical value from the canonical
//!   storage ([`Context::projected_value`]);
//! - everything else REVEALS the cell (gives it back its logical arm), runs
//!   the unchanged operation on it, and conceals it again
//!   ([`Context::with_revealed_projection`]).
//!
//! Each entry is cold and out of line. Round 3 replaces the binding paths
//! with owner-local slots; the places that will change are marked
//! "ROUND 3".

use super::{Context, EvalResult, Flow, LispCondition, SymbolValueLookup, signal};
use crate::emacs_core::forward::{CanonicalArm, LispBufferObjFwd, ThreadProjectionDescriptor};
use crate::emacs_core::intern::SymId;
use crate::emacs_core::symbol::SetInternalBind;
use crate::emacs_core::value::Value;

#[cfg(test)]
thread_local! {
    /// How many `LetLocal` entries `specbind` pushed for a projected
    /// variable whose logical arm is `Localized`: the one binding shape
    /// whose restore waits for round 3's owner-binding entry.
    pub(crate) static ROUND3_LOCALIZED_LET_LOCAL: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

impl Context {
    /// GNU `find_symbol_value` of the variable PROJECTION wraps, for symbol
    /// ID in the current buffer.
    #[cold]
    #[inline(never)]
    pub(crate) fn projected_value(
        &self,
        id: SymId,
        projection: &ThreadProjectionDescriptor,
    ) -> SymbolValueLookup {
        // ROUND 3: an owner's binding answers first.
        let value = match projection.arm() {
            CanonicalArm::Plain => projection.plain_value(),
            CanonicalArm::Localized(blv) => {
                self.obarray
                    .read_projected_localized(id, blv, self.buffers.current_buffer())
            }
            CanonicalArm::Forwarded(builtin) => {
                let fwd = builtin.descriptor();
                match fwd.load() {
                    Some(value) => value,
                    None => match fwd.as_buffer_obj_fwd() {
                        Some(buf_fwd) => self.builtin_per_buffer_value(buf_fwd),
                        None => Value::UNBOUND,
                    },
                }
            }
        };
        if value.is_unbound() {
            SymbolValueLookup::Unbound
        } else {
            SymbolValueLookup::Bound(value)
        }
    }

    /// `Bvarref`'s void branch: NAME (resolving to RESOLVED) read UNBOUND.
    /// A projected variable answers its canonical value; anything else, and
    /// a void projected variable, signals `void-variable`.
    #[cold]
    #[inline(never)]
    pub(crate) fn projected_value_or_void(&self, name: SymId, resolved: SymId) -> EvalResult {
        if let Some(projection) = self.obarray.thread_projection_of(resolved)
            && let SymbolValueLookup::Bound(value) = self.projected_value(resolved, projection)
        {
            return Ok(value);
        }
        Err(signal(
            LispCondition::VoidVariable,
            vec![Value::from_sym_id(name)],
        ))
    }

    /// The current value of a real per-buffer forwarder.
    fn builtin_per_buffer_value(&self, buf_fwd: &LispBufferObjFwd) -> Value {
        match buf_fwd.buffer_slot() {
            Some(slot) => self.per_buffer_value(buf_fwd, slot),
            None => Value::UNBOUND,
        }
    }

    /// Run OP with ID's projected cell revealed as the variable it wraps,
    /// then conceal it again over whatever logical arm OP left. ID must be
    /// projected; OP sees exactly the unprojected variable.
    #[inline]
    pub(crate) fn with_revealed_projection<R>(
        &mut self,
        id: SymId,
        op: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let revealed = self.reveal_projection(id);
        let result = op(self);
        if let Some(revealed) = revealed {
            self.conceal_projection(revealed);
        }
        result
    }

    #[cold]
    #[inline(never)]
    fn reveal_projection(
        &mut self,
        id: SymId,
    ) -> Option<crate::emacs_core::symbol::RevealedProjection> {
        self.obarray.reveal_projection(id)
    }

    #[cold]
    #[inline(never)]
    fn conceal_projection(&mut self, revealed: crate::emacs_core::symbol::RevealedProjection) {
        // An alias retires the projection; the variable it names is
        // projected on its own (ROUND 3: admission projects bases).
        let _ = self.obarray.conceal_projection(revealed);
    }

    /// `specbind` of a projected variable: GNU's shallow binding on the
    /// variable it projects. The specpdl entry it pushes names the symbol,
    /// and unbinding restores through the same routing.
    /// ROUND 3: an owner binds in its own slot instead.
    #[cold]
    #[inline(never)]
    pub(super) fn specbind_projected(&mut self, resolved: SymId, value: Value) -> Result<(), Flow> {
        #[cfg(test)]
        let localized = matches!(
            self.obarray.thread_projection_of(resolved).map(|p| p.arm()),
            Some(CanonicalArm::Localized(_))
        );
        let result =
            self.with_revealed_projection(resolved, |ctx| ctx.specbind_uncached(resolved, value));
        // ROUND 3: the unbind loop's LetLocal arm tests the cell's redirect,
        // so it cannot restore a concealed localized variable's buffer
        // binding; the owner-binding entry kind replaces this push.
        #[cfg(test)]
        if localized
            && matches!(
                self.specpdl.last(),
                Some(super::SpecBinding::LetLocal { sym_id, .. }) if *sym_id == resolved
            )
        {
            ROUND3_LOCALIZED_LET_LOCAL.with(|count| count.set(count.get() + 1));
        }
        result
    }

    /// `set_internal` (bind, unbind, thread switch) of a projected variable,
    /// on the variable it projects.
    #[cold]
    #[inline(never)]
    pub(super) fn set_projected_internal(
        &mut self,
        sym_id: SymId,
        resolved: SymId,
        value: Value,
        bindflag: SetInternalBind,
    ) -> Result<(), Flow> {
        self.with_revealed_projection(resolved, |ctx| {
            ctx.set_internal_after_watchers(sym_id, value, bindflag)
        })
    }

    /// Panic-recovery restore of a projected variable's binding.
    #[cold]
    #[inline(never)]
    pub(super) fn discard_restore_projected(&mut self, resolved: SymId, saved: Option<Value>) {
        self.with_revealed_projection(resolved, |ctx| {
            ctx.discard_restore_default_binding(resolved, saved)
        });
    }

    /// `variable_watcher_where_for_set_by_id` of a projected variable: the
    /// buffer a `set` of the variable it projects would land in.
    #[cold]
    #[inline(never)]
    pub(super) fn projected_watcher_where(
        &self,
        id: SymId,
        projection: &ThreadProjectionDescriptor,
        current: crate::buffer::BufferId,
    ) -> Value {
        let in_buffer = match projection.arm() {
            CanonicalArm::Plain => false,
            CanonicalArm::Localized(blv) => {
                // SAFETY: a projection's localized arm names a live record
                // of the obarray's BLV pool; one flag is read.
                let local_if_set = unsafe { (*blv.blv().as_ptr()).local_if_set };
                local_if_set
                    || self
                        .buffers
                        .get(current)
                        .is_some_and(|buf| buf.has_buffer_local_by_sym_id(id))
            }
            CanonicalArm::Forwarded(builtin) => builtin.descriptor().as_buffer_obj_fwd().is_some(),
        };
        if in_buffer {
            Value::make_buffer(current)
        } else {
            Value::NIL
        }
    }
}
