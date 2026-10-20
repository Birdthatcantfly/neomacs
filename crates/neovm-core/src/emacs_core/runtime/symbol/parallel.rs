//! Narrow backing for proof-3 authority. No serial accessor calls this module.
#![allow(dead_code)]

use super::{Obarray, SymbolTrappedWrite, ValueCell};
use crate::emacs_core::forward::{CanonicalArm, ThreadProjectionTable};
use crate::emacs_core::intern::SymId;
use std::cell::RefCell;
use std::ptr::NonNull;

/// Metadata only: no Value, cell address or descriptor escapes admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlainSymbolBacking {
    Cell,
    Projected,
    VoidProjection,
}

/// Original World allocation roots, never references derived from a Context
/// view. The era retains the World's Baton even on abandoned/poisoned teardown.
pub(crate) struct SymbolBacking {
    obarray: NonNull<Obarray>,
    projections: NonNull<RefCell<ThreadProjectionTable>>,
    pub(crate) world: usize,
    pub(crate) generation: u64,
}

// SAFETY: only the keeper constructs this bridge from retained World roots.
// All dereferences require the era's exclusive word or matching AllStopped;
// no reference, Value or RefCell loan leaves this module. Read leases in C2
// use already-admitted metadata and never borrow the shared RefCell. This is
// not a Send/Sync implementation for Obarray, FwdRef, a descriptor or World.
unsafe impl Send for SymbolBacking {}
unsafe impl Sync for SymbolBacking {}

impl SymbolBacking {
    /// # Safety
    /// Roots belong to the same live World, all ordinary views are escrowed,
    /// and the keeper retains that World until all era access ends. All calls
    /// to classify must hold the symbol writer or matching stop-all authority.
    pub(crate) unsafe fn new(
        obarray: NonNull<Obarray>,
        projections: NonNull<RefCell<ThreadProjectionTable>>,
        world: usize,
    ) -> Self {
        // SAFETY: construction is serial, with the original World retained.
        let generation = unsafe { obarray.as_ref() }.generation();
        Self {
            obarray,
            projections,
            world,
            generation,
        }
    }

    /// # Safety
    /// Exclusive symbol authority, or matching AllStopped, covers every read
    /// and the RefCell borrow. Shared readers must use admitted metadata.
    pub(crate) unsafe fn classify(&self, id: SymId) -> Option<PlainSymbolBacking> {
        // SAFETY: caller's exclusive authority; no whole mutable aggregate.
        let obarray = unsafe { self.obarray.as_ref() };
        if obarray.generation() != self.generation {
            return None;
        }
        let symbol = obarray.get_by_id(id)?;
        if symbol.trapped_write() != SymbolTrappedWrite::Untrapped {
            return None;
        }
        match symbol.value_cell() {
            ValueCell::Plain(_) if !symbol.flags().runtime_projected() => {
                Some(PlainSymbolBacking::Cell)
            }
            ValueCell::Forwarded(fwd) => {
                // SAFETY: same retained World, exclusive admission/keeper.
                let table = unsafe { self.projections.as_ref() }.try_borrow().ok()?;
                let projection = table.projection(fwd)?;
                if projection.host_projected() || projection.arm() != CanonicalArm::Plain {
                    return None;
                }
                Some(if projection.is_void() {
                    PlainSymbolBacking::VoidProjection
                } else {
                    PlainSymbolBacking::Projected
                })
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) unsafe fn probe(&self, id: SymId) -> usize {
        // SAFETY: the test holds the real read/write/keeper lease. Only the
        // already-admitted plain backing is read; no RefCell borrow or TLS.
        let symbol = unsafe { self.obarray.as_ref() }.get_by_id(id).unwrap();
        match symbol.value_cell() {
            ValueCell::Plain(value) => value.bits(),
            ValueCell::Forwarded(fwd) => fwd.thread_projection().unwrap().plain_value().bits(),
            _ => panic!("test probe requires admitted Plain backing"),
        }
    }
}

#[cfg(test)]
#[path = "tests/parallel_test.rs"]
mod tests;
