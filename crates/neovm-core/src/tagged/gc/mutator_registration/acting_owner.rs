//! The acting-owner TLS: which owner's counters a sealed edit's storage
//! growth is charged to (design §4 Rev R3, §7 B5).
//!
//! Storage growth happens at the two Vec sites the serial path already
//! uses — `HashTableStorage::alloc_slot` and `HashIndex::insert_new` — so
//! the sealed path cannot drift from Sim pacing (Rev B1-Q3). Those sites
//! run under whichever code path called them; only a live [`TableMut`]
//! edit has an acting owner, installed here for exactly the extent of the
//! edit. With no acting owner (every serial/Sim execution, and the
//! keeper's stop-all edits, which bill no one) [`note_hash_table_storage_growth`]
//! is a no-op: Sim's growth stays exactly today's code path.

use std::cell::Cell;
use std::ptr::addr_of_mut;

use super::super::mutator_gc::MutatorGcState;
use super::token::Mutator;

thread_local! {
    /// The acting owner's `hot` GC state, or null when the current edit is
    /// not a sealed owner edit. Raw because the guard's lifetime is nested
    /// inside the lease that borrowed the owner, which keeps the state (a
    /// boxed field) alive and immovable for that whole extent.
    static ACTING_OWNER_GC: Cell<*mut MutatorGcState> = const { Cell::new(std::ptr::null_mut()) };
}

/// The pointer a [`TableMut`] mint should install around its edit body.
///
/// Null when the owner carries no local GC state (it has not yet run): a
/// growth charge during such an edit bills no one, matching how the
/// registry sums only states that exist.
pub(super) fn owner_gc_ptr(owner: &mut Mutator<'_>) -> *mut MutatorGcState {
    owner
        .local
        .as_deref_mut()
        .map_or(std::ptr::null_mut(), |state| {
            // SAFETY: `state` is a `&mut OwnerGcState` (the Box deref), so
            // `hot` is initialized and the borrow excludes every other
            // access for its duration.
            unsafe { addr_of_mut!(state.hot) }
        })
}

/// RAII install of the acting owner. Constructed with the pointer from
/// [`owner_gc_ptr`]; restoring the previous value on drop keeps installs
/// nested correctly should an edit body ever run another edit.
pub(super) struct ActingOwnerGuard {
    previous: *mut MutatorGcState,
}

impl ActingOwnerGuard {
    pub(super) fn new(owner: *mut MutatorGcState) -> Self {
        let previous = ACTING_OWNER_GC.with(|slot| slot.replace(owner));
        Self { previous }
    }
}

impl Drop for ActingOwnerGuard {
    fn drop(&mut self) {
        ACTING_OWNER_GC.with(|slot| slot.set(self.previous));
    }
}

/// Charge `bytes` of hash-table storage growth to the acting owner's
/// GC counters, or to no one when there is no acting owner.
///
/// Called from the two shared Vec-growth sites; a no-op everywhere outside
/// a live sealed edit (the whole serial/Sim world), so only sealed-path
/// growth is ever attributed.
pub(crate) fn note_hash_table_storage_growth(bytes: usize) {
    if bytes == 0 {
        return;
    }
    let owner = ACTING_OWNER_GC.with(|slot| slot.get());
    if owner.is_null() {
        return;
    }
    // SAFETY: a non-null pointer here was installed by an `ActingOwnerGuard`
    // still on this thread's stack (its drop has not run), and the guard's
    // lease keeps the owning `Mutator` — and so the boxed state `hot` lives
    // in — borrowed and immovable for the guard's whole extent.
    unsafe { (*owner).bytes_since_gc += bytes };
}
