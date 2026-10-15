//! GNU-global state that lives in thread-locals (round 3w, plan-3w.md
//! section 7, C1/C2): the Baton holder's thread-locals hold the World's
//! value, and the World holds it while the holder is parked. A park takes it
//! out of the parking thread's thread-locals; the next resume (or a new
//! thread's start) puts it back, on whichever OS thread resumes. Sim Worlds
//! never move it, so their thread-local fast paths are unchanged.

use std::any::Any;

type Take = fn() -> Box<dyn Any>;
type Restore = fn(Box<dyn Any>);

/// Every carried piece of World state, with its GNU anchor.
const CARRIERS: &[(Take, Restore)] = &[
    // composite.c `composition_table`.
    (
        crate::emacs_core::composite::take_world_state,
        crate::emacs_core::composite::restore_world_state,
    ),
    // process.c process names (`Vprocess_alist`).
    (
        crate::emacs_core::process::take_world_state,
        crate::emacs_core::process::restore_world_state,
    ),
    // sqlite.c connections and statements (Lisp objects, global).
    (
        crate::emacs_core::sqlite::take_world_state,
        crate::emacs_core::sqlite::restore_world_state,
    ),
    // dbusbind.c buses and registered fds.
    (
        crate::emacs_core::dbusbind::take_world_state,
        crate::emacs_core::dbusbind::restore_world_state,
    ),
    // The clipboard, the primary selection and the monitor list.
    (
        crate::emacs_core::builtins::take_selection_world_state,
        crate::emacs_core::builtins::restore_selection_world_state,
    ),
    // keyboard.c `lossage_limit`.
    (
        crate::emacs_core::builtins::symbols::take_world_state,
        crate::emacs_core::builtins::symbols::restore_world_state,
    ),
    // undo.c's compaction latch.
    (
        crate::emacs_core::undo::take_world_state,
        crate::emacs_core::undo::restore_world_state,
    ),
    // terminal.c `terminal_list`.
    (
        crate::emacs_core::terminal::pure::take_world_state,
        crate::emacs_core::terminal::pure::restore_world_state,
    ),
    // alloc.c consing counters (`memory-use-counts`).
    (
        crate::emacs_core::value::take_world_state,
        crate::emacs_core::value::restore_world_state,
    ),
    // The collection-write revision (observation epoch).
    (
        crate::tagged::mutate::take_world_state,
        crate::tagged::mutate::restore_world_state,
    ),
    // Collection-read capture state (observation).
    (
        crate::tagged::collection_reads::take_world_state,
        crate::tagged::collection_reads::restore_world_state,
    ),
    // Property-list write revision (observation).
    (
        crate::emacs_core::symbol::take_property_revision_world_state,
        crate::emacs_core::symbol::restore_property_revision_world_state,
    ),
    // Display-table and window-parameter layout epochs.
    (
        crate::window::take_display_world_state,
        crate::window::restore_display_world_state,
    ),
    // The time-zone rule (GNU's TZ, process-wide).
    (
        crate::emacs_core::timefns::take_world_state,
        crate::emacs_core::timefns::restore_world_state,
    ),
];

/// The World's thread-local state while its holder is parked.
pub(crate) struct CarriedState(Vec<Box<dyn Any>>);

impl std::fmt::Debug for CarriedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CarriedState").field(&self.0.len()).finish()
    }
}

impl CarriedState {
    /// Take the World's state out of this OS thread's thread-locals.
    #[cold]
    pub(crate) fn take() -> Self {
        Self(CARRIERS.iter().map(|(take, _)| take()).collect())
    }

    /// Put the World's state into this OS thread's thread-locals.
    #[cold]
    pub(crate) fn restore(self) {
        for ((_, restore), state) in CARRIERS.iter().zip(self.0) {
            restore(state);
        }
    }
}
