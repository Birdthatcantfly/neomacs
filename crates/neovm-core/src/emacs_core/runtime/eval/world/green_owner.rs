//! Green owner construction (Phase 0 step 2, round 3d): further Contexts
//! over one World, one per Lisp thread, each on its own OS thread.
//!
//! # Safety argument
//!
//! The sole-owner policy (`sole_owner`) proves World views safe for ONE
//! Context: each manager cell has exactly one mutable view, owned by that
//! Context. A Green World admits further owners, each with its own full set
//! of views over the same cells, so that proof no longer holds by itself.
//! It is replaced by the [`Baton`]:
//!
//! 1. A Green World has exactly one Baton, issued once
//!    ([`Context::issue_green_baton`]). It is `Send` and neither `Sync` nor
//!    `Clone`, so at any time exactly one OS thread holds it.
//! 2. Only the holder runs Lisp or touches any view of the World: an owner
//!    builds its views only from the Baton it holds
//!    ([`Baton::owner_views`]), and it runs only between receiving the Baton
//!    and handing it on. Every other owner is parked, blocked in its
//!    receive, holding views it does not dereference.
//! 3. Handing the Baton on is a channel send, received by the next holder:
//!    the send/receive pair orders every write the previous holder made
//!    through its views before every access the next holder makes. That is
//!    the only cross-thread edge any World cell ever crosses.
//! 4. The World's `Cell`s and `RefCell`s, its heap and its obarray are thus
//!    accessed by one thread at a time, in a total order, which is what
//!    their `!Sync` contracts require. World and Context stay `!Send`; only
//!    the Baton carries the World's allocation across threads.
//! 5. The Baton also retains the heap's facade epoch, so no concurrent
//!    marker runs while Green owners exist: every collection is synchronous,
//!    on the holder.
//!
//! Debug builds check point 2 at every view creation: the World records its
//! holder's OS thread ([`Baton::enter`]), and [`Baton::owner_views`] panics
//! off that thread.

use super::*;

/// The right to run Lisp in a Green World: whoever holds it is the one OS
/// thread that may touch the World. See the module's safety argument.
#[must_use = "the World's Lisp stops while its Baton is dropped"]
pub(crate) struct Baton {
    world: Arc<World>,
    /// Keeps concurrent marking excluded for the Green era (point 5).
    _retention: crate::tagged::gc::FacadeEpochRetention,
}

// SAFETY: the Baton is the World's single, unshareable token (point 1); the
// World behind it is touched only by the thread holding it, and the channel
// that moves it orders those accesses (points 2-4).
unsafe impl Send for Baton {}
static_assertions::assert_not_impl_any!(Baton: Sync, Clone, Copy);

impl fmt::Debug for Baton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Baton")
            .field("holder", &self.world.baton_holder.get())
            .finish_non_exhaustive()
    }
}

impl Baton {
    /// The calling OS thread has received the Baton: it is the holder.
    pub(crate) fn enter(&self) {
        self.world
            .baton_holder
            .set(Some(std::thread::current().id()));
    }

    /// Whether the calling OS thread holds the Baton.
    pub(crate) fn held_here(&self) -> bool {
        self.world.baton_holder.get() == Some(std::thread::current().id())
    }

    /// A further owner's views of the World, starting in CURRENT. Only the
    /// holder may build them (point 2, checked in debug builds).
    pub(in crate::emacs_core::eval) fn owner_views(&self, current: Option<BufferId>) -> ReadyWorld {
        debug_assert!(
            self.held_here(),
            "a World view made off the thread holding the World's Baton"
        );
        let lease = WorldLease(Arc::clone(&self.world));
        WorldStorageBuilder::publish_views(&lease, BufferSelection::for_owner(current))
    }

    /// Reserve a parked root publication under this World's held exclusion.
    pub(in crate::emacs_core::eval) fn prepare_parked_batch(
        &self,
        heap: &crate::tagged::gc::TaggedHeap,
        capacity: usize,
    ) -> Result<crate::tagged::transport::PreparedRootBatch, crate::tagged::transport::RootBatchError>
    {
        debug_assert!(self.held_here(), "root publication prepared off the Baton");
        crate::tagged::transport::PreparedRootBatch::reserve_retained(
            heap,
            self._retention.retain()?,
            capacity,
        )
    }

    /// The World's Green runtime.
    pub(crate) fn green_runtime(
        &self,
    ) -> std::cell::Ref<'_, Option<Box<super::super::green::GreenRuntime>>> {
        debug_assert!(self.held_here(), "the Green runtime read off the Baton");
        self.world.green.borrow()
    }
}

impl Context {
    /// Issue this Green World's one Baton to the calling thread, the
    /// owner's: the collector is drained and its facade epoch retained for
    /// the Green era, so no concurrent marker runs while Green owners exist.
    #[cold]
    #[inline(never)]
    pub(in crate::emacs_core::eval) fn issue_green_baton(
        &mut self,
    ) -> Result<Baton, super::super::green::GreenStartError> {
        let heap: *mut crate::tagged::gc::TaggedHeap = &mut *self.tagged_heap;
        // SAFETY: the sole running owner, at a safe point, with no manager
        // loan or callback live; the reseed enumerates the registered root
        // census and only adds mark roots (as admission's collector-closed
        // phase does).
        let exclusion = unsafe {
            (*heap)
                .drain_to_quiescent(|heap| {
                    self.seed_registered_mutator_roots_world_stopped(heap);
                })?
                .exclude_concurrent_mark()?
        };
        let retention = exclusion.retain()?;
        drop(exclusion);
        let world = Arc::clone(&self.obarray.lease.0);
        world.baton_holder.set(Some(std::thread::current().id()));
        Ok(Baton {
            world,
            _retention: retention,
        })
    }

    /// This World's Green runtime, if it started.
    pub(in crate::emacs_core::eval) fn green_runtime_ref(
        &self,
    ) -> std::cell::Ref<'_, Option<Box<super::super::green::GreenRuntime>>> {
        self.obarray.lease.0.green.borrow()
    }

    /// Install the World's Green runtime (once).
    pub(in crate::emacs_core::eval) fn install_green_runtime(
        &self,
        runtime: Box<super::super::green::GreenRuntime>,
    ) {
        let mut slot = self.obarray.lease.0.green.borrow_mut();
        debug_assert!(slot.is_none(), "a World starts its Green runtime once");
        *slot = Some(runtime);
    }
}
