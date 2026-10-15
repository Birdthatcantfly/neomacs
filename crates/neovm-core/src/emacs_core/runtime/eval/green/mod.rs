//! The Green runtime (Phase 0 step 2, round 3d): GNU's Lisp threads as one
//! OS thread per Lisp thread, over one World, one running at a time.
//!
//! A Green World ([`ThreadingMode::Green`](super::ThreadingMode)) starts its
//! runtime at the first `make-thread`. That admits threads (round 3c), then
//! captures what a further owner is built from ([`GreenBootstrap`]) and
//! issues the World's [`Baton`](super::world::Baton). Each Lisp thread then
//! runs on its own OS thread, with its own [`Context`] over the same World
//! views: its own specpdl, handlers and current buffer, and its own owner
//! slab of dynamic bindings. Only the thread holding the Baton runs Lisp.
//!
//! What a further owner shares, copies or starts fresh is the
//! thread-local audit in design-r3.md §11.

mod api;
mod bootstrap;
mod scheduler;
mod switch;

pub(crate) use api::{green_make_thread, green_world};
pub(crate) use bootstrap::GreenBootstrap;
#[cfg(debug_assertions)]
pub(crate) use bootstrap::debug_assert_native_tables_open;
pub(crate) use scheduler::{ThreadKey, Wait};
pub(crate) use switch::{GreenSeat, SelectTurn};

use super::admission::AdmissionError;
use super::{Context, ThreadingMode};
use crate::tagged::gc::{CollectorQuiescenceError, FacadeMarkExclusionError};

/// The World's Green runtime, once started: what further owners are built
/// from, the scheduler, and the state every Lisp thread shares. Held by the
/// World and traced with it.
pub(crate) struct GreenRuntime {
    pub(super) bootstrap: GreenBootstrap,
    pub(super) control: std::sync::Arc<scheduler::Control>,
    /// Each parked thread's private roots (plan-3d §4).
    pub(super) parked:
        std::cell::RefCell<rustc_hash::FxHashMap<ThreadKey, Vec<crate::emacs_core::value::Value>>>,
    /// A started thread's function until its OS thread runs it.
    pub(super) pending_starts:
        std::cell::RefCell<rustc_hash::FxHashMap<ThreadKey, crate::emacs_core::value::Value>>,
    /// A `kill-emacs` run by another thread, for main to exit with.
    pub(super) exit: std::cell::Cell<Option<super::ShutdownRequest>>,
    /// The standard case table, one for every thread as in GNU:
    /// `set-standard-case-table` sets it here, and each thread installs it
    /// as it resumes.
    pub(super) standard_case_table: std::cell::Cell<Option<crate::emacs_core::value::Value>>,
    /// An error signalled into a thread before it first ran, as
    /// `(SYMBOL, DATA)`: held from its start to its first switch, as GNU
    /// `post_acquire_global_lock` raises nothing while a thread has no
    /// handlers.
    pub(super) deferred_errors: std::cell::RefCell<
        rustc_hash::FxHashMap<
            ThreadKey,
            (
                crate::emacs_core::value::Value,
                crate::emacs_core::value::Value,
            ),
        >,
    >,
}

impl std::fmt::Debug for GreenRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GreenRuntime").finish_non_exhaustive()
    }
}

impl GreenRuntime {
    /// The process exit KEY takes on resuming, if any: main exits with a
    /// `kill-emacs` another thread ran, and every other thread ends once
    /// main has ended.
    pub(super) fn exit_for(
        &self,
        key: ThreadKey,
        control: &scheduler::Control,
    ) -> Option<super::ShutdownRequest> {
        match key {
            ThreadKey::Main => self.exit.take(),
            ThreadKey::Worker(_) => {
                (control.lifecycle() == scheduler::Lifecycle::Closing).then(|| {
                    self.exit.get().unwrap_or(super::ShutdownRequest {
                        exit_code: 0,
                        restart: false,
                    })
                })
            }
        }
    }

    /// Visit every value the runtime holds. Out of line: the World's trace
    /// reaches it only in a Green World.
    #[cold]
    #[inline(never)]
    pub(super) fn trace_roots(&self, visit: &mut dyn FnMut(crate::emacs_core::value::Value)) {
        self.bootstrap.trace_roots(visit);
        for values in self.parked.borrow().values() {
            for value in values {
                visit(*value);
            }
        }
        for value in self.pending_starts.borrow().values() {
            visit(*value);
        }
        if let Some(table) = self.standard_case_table.get() {
            visit(table);
        }
        for (symbol, data) in self.deferred_errors.borrow().values() {
            visit(*symbol);
            visit(*data);
        }
    }
}

/// Why the Green runtime could not start, or a Green operation refused.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GreenStartError {
    #[error("the World does not run green Lisp threads")]
    NotGreen,
    #[error(
        "green Lisp threads need the interpreter: run with NEOVM_JIT=0 \
         (compiled continuations across thread switches are not supported yet)"
    )]
    JitEnabled,
    #[error("green Lisp threads are unavailable while a display or input host is attached")]
    ThreadAffineHost,
    #[error("green Lisp threads cannot start while garbage collection is inhibited")]
    GcInhibited,
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    #[error(transparent)]
    Collector(#[from] CollectorQuiescenceError),
    #[error(transparent)]
    MarkExclusion(#[from] FacadeMarkExclusionError),
}

/// Whether compiled code may run: Green owners refuse it (round 3d2 ports
/// compiled continuations across switches).
fn jit_enabled() -> bool {
    #[cfg(feature = "jit")]
    {
        crate::emacs_core::jit::jit_runtime_enabled()
    }
    #[cfg(not(feature = "jit"))]
    {
        false
    }
}

impl Context {
    /// Start this World's Green runtime from its first owner: admit
    /// threads, capture the bootstrap further owners are built from, and
    /// issue the World's Baton to this thread. Refused unless the World is
    /// Green, the JIT is off, no display or input host is attached and
    /// collection is not inhibited.
    #[cold]
    #[inline(never)]
    pub(crate) fn start_green_runtime(&mut self) -> Result<super::world::Baton, GreenStartError> {
        if self.threading_mode() != ThreadingMode::Green {
            return Err(GreenStartError::NotGreen);
        }
        if jit_enabled() {
            return Err(GreenStartError::JitEnabled);
        }
        if self.display_host.is_some() || self.input_rx.is_some() {
            return Err(GreenStartError::ThreadAffineHost);
        }
        if self.gc_inhibit_depth != 0 {
            return Err(GreenStartError::GcInhibited);
        }
        if !self.threads_admitted() {
            self.admit_threads()?;
        }
        let bootstrap = GreenBootstrap::capture(self);
        let baton = self.issue_green_baton()?;
        self.install_green_runtime(Box::new(GreenRuntime {
            bootstrap,
            control: Default::default(),
            parked: Default::default(),
            pending_starts: Default::default(),
            exit: Default::default(),
            standard_case_table: std::cell::Cell::new(self.cached_standard_case_table),
            deferred_errors: Default::default(),
        }));
        Ok(baton)
    }

    /// A further owner of the World BATON belongs to, on this OS thread,
    /// which holds the Baton: its own execution state over the World's
    /// views, starting in the creator's current buffer, with the World's
    /// canonical registries and the startup subr tables.
    #[cold]
    #[inline(never)]
    pub(crate) fn new_green_owner(baton: &super::world::Baton) -> Context {
        let (seed, current, case_table) = {
            let runtime = baton.green_runtime();
            let bootstrap = &runtime
                .as_ref()
                .expect("a started Green runtime issued this Baton")
                .bootstrap;
            bootstrap.install_native_tables();
            (
                bootstrap.seed(),
                bootstrap.current_buffer,
                runtime
                    .as_ref()
                    .and_then(|runtime| runtime.standard_case_table.get()),
            )
        };
        let mut views = baton.owner_views(current);
        crate::tagged::gc::set_tagged_heap(&mut views.tagged_heap);
        let mut owner = Context::assemble(views, seed);
        owner.cached_standard_case_table = case_table;
        owner.gc_count = owner.tagged_heap.gc_collections() as u64;
        owner.ensure_owner_slab();
        owner.setup_thread_locals();
        owner.republish_owner_mirrors();
        #[cfg(all(test, feature = "jit"))]
        crate::emacs_core::jit::force_jit_off_for_test(true);
        owner
    }

    /// Retire a further owner on its own OS thread, holding the Baton: its
    /// thread-local installations go; the World's shared registries stay
    /// (a Context's drop clears its dynamic-module registry, which a
    /// further owner shares with the rest of the World).
    #[cold]
    #[inline(never)]
    pub(crate) fn retire_green_owner(mut self) {
        self.dynamic_module_registry =
            super::super::dynamic_module::DynamicModuleRegistryHandle::new(Vec::new());
        drop(self);
    }
}

#[cfg(test)]
#[path = "../tests/green_owner_test.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/green_switch_test.rs"]
mod switch_tests;
