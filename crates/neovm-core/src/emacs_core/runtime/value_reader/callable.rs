//! Scratch-root scope used by the value reader.

use super::super::eval::{restore_scratch_gc_roots, save_scratch_gc_roots};
use std::marker::PhantomData;

/// A mutator-local root scope restores scratch roots on every exit path.
/// Drop never calls Lisp, blocks, or panics.
#[must_use = "dropping the scope restores the mutator scratch roots"]
#[derive(Debug)]
pub(super) struct RootScope {
    saved: usize,
    confined: PhantomData<*const ()>,
}
impl RootScope {
    pub(super) fn new() -> Self {
        let saved = save_scratch_gc_roots();
        Self {
            saved,
            confined: PhantomData,
        }
    }
}
impl Drop for RootScope {
    fn drop(&mut self) {
        restore_scratch_gc_roots(self.saved);
    }
}
static_assertions::assert_not_impl_any!(RootScope: Send, Sync);
