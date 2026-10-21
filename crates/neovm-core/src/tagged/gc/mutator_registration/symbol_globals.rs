//! Test-only C3 executor surface. Real reader operations live in the sealed
//! authority; the remaining writer stubs are replaced in the next storage batch.
use super::{GlobalError, OwnerRootSlot, RunAccess, SymbolHandle};

/// Closed executor surface. `symbol` indexes serially admitted real World
/// handles, not arbitrary symbol values; all other indices name owner roots.
/// Integrate with restricted_lisp::Op/Flow::Lease after b143, without opening
/// an executor route to legacy Obarray/Context accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GlobalOp {
    Value { symbol: u16, dst: u16 },
    Set { symbol: u16, src: u16, dst: u16 },
    Boundp { symbol: u16, dst: u16 },
    Makunbound { symbol: u16, dst: u16 },
}

impl<'era> RunAccess<'_, 'era> {
    /// Writer lease: read src before any destination write, update the real
    /// canonical backing, and root the assigned value even if later overwritten.
    pub(super) fn sym_set_into(
        &mut self,
        _symbol: &SymbolHandle,
        _src: &OwnerRootSlot<'era>,
        _dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), GlobalError> {
        Err(GlobalError::AwaitingB143)
    }

    /// Writer lease: store UNBOUND and root the original symbol as the result.
    pub(super) fn sym_makunbound_into(
        &mut self,
        _symbol: &SymbolHandle,
        _dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), GlobalError> {
        Err(GlobalError::AwaitingB143)
    }
}

#[cfg(not(feature = "gc-memory-telemetry"))]
mod tests;
