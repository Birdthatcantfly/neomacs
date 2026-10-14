//! Thread admission (Phase 0 step 2, round 3c): the one-time, sticky move of
//! a World from one dormant owner to owner-local dynamic bindings.

/// How a World runs Lisp threads, fixed at its construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadingMode {
    /// `make-thread` simulates threads in the one owner, swapping dynamic
    /// bindings in place (`threads`); there is no admission.
    Sim,
    /// Lisp threads are owners with owner-local bindings, after admission.
    Green,
}

use super::Context;

impl Context {
    /// A Context whose World runs Lisp threads in MODE, fixed before any
    /// Lisp runs. [`Context::new`] is `Sim`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new_with_threading(mode: ThreadingMode) -> Self {
        let mut ctx = Self::new();
        ctx.set_threading_mode_at_construction(mode);
        ctx
    }
}
