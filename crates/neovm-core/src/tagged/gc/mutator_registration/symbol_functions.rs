//! C4 restricted call syntax and remaining preparation bodies. Direct
//! function reads use the production sealed root-to-root API.
use super::{FunctionError, FunctionOpKind, OwnerRootSlot, RunAccess, SymbolHandle};
use std::num::NonZeroU32;

/// Cache payload remains in this owner's existing root census. Epoch/symbol
/// are metadata only, never execution or lifetime authority on their own.
pub(super) struct FunctionCallCache<'era> {
    home: OwnerRootSlot<'era>,
    symbol: Option<SymbolHandle>,
    observed_epoch: Option<u64>,
}
impl<'era> FunctionCallCache<'era> {
    pub(super) fn empty(home: OwnerRootSlot<'era>) -> Self {
        Self {
            home,
            symbol: None,
            observed_epoch: None,
        }
    }
}

/// The real definition must be rooted in home before selection releases its
/// reader. Resumes consume finite fuel outside symbol authority. Clearing
/// the root bank invalidates this token just like every other owner home.
pub(super) struct FunctionActivation<'era> {
    home: OwnerRootSlot<'era>,
    selected_epoch: Option<u64>,
    instruction: u32,
}
impl<'era> FunctionActivation<'era> {
    pub(super) fn empty(home: OwnerRootSlot<'era>) -> Self {
        Self {
            home,
            selected_epoch: None,
            instruction: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FunctionStep {
    Yielded,
    Returned,
}

/// Closed syntax only: handles and resident home indices, no raw callee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FunctionOp {
    Function { symbol: u16, dst: u16 },
    Fset { symbol: u16, src: u16, dst: u16 },
    Fmakunbound { symbol: u16, dst: u16 },
    CallBegin { symbol: u16 },
    CallStep { dst: u16, fuel: NonZeroU32 },
}

impl<'era> RunAccess<'_, 'era> {
    pub(super) fn sym_call_begin(
        &mut self,
        _symbol: &SymbolHandle,
        _cache: &mut FunctionCallCache<'era>,
        _activation: &mut FunctionActivation<'era>,
    ) -> Result<(), FunctionError> {
        Err(FunctionError::MissingOperation(FunctionOpKind::CallBegin))
    }
    pub(super) fn sym_call_step(
        &mut self,
        _activation: &mut FunctionActivation<'era>,
        _dst: &mut OwnerRootSlot<'era>,
        _fuel: NonZeroU32,
    ) -> Result<FunctionStep, FunctionError> {
        Err(FunctionError::MissingOperation(FunctionOpKind::CallStep))
    }
}

static_assertions::assert_not_impl_any!(FunctionCallCache<'static>: Send, Sync, Clone, Copy);
static_assertions::assert_not_impl_any!(FunctionActivation<'static>: Send, Sync, Clone, Copy);

#[cfg(not(feature = "gc-memory-telemetry"))]
mod tests;
