//! C3 preparation contract, compiled ONLY for tests. Storage implementation
//! waits for b143. These explicit unavailable bodies are intentional red tests,
//! not a working executor or an alternative symbol store.
use super::{OwnerRootSlot, RegistrationError, RunAccess, SymbolHandle};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GlobalError {
    Authority(RegistrationError),
    VoidVariable,
    /// Preparation only. Remove when the real storage implementation lands.
    AwaitingB143,
}

impl<'era> RunAccess<'_, 'era> {
    /// Reader lease: copy into this owner's root before releasing authority.
    pub(super) fn sym_value_into(
        &mut self,
        _symbol: &SymbolHandle,
        _dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), GlobalError> {
        Err(GlobalError::AwaitingB143)
    }

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

    /// Reader lease: install Lisp nil/t, distinguishing a nil value from void.
    pub(super) fn sym_boundp_into(
        &mut self,
        _symbol: &SymbolHandle,
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
