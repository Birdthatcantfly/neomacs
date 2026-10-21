//! The era's sealed, marker-excluded store seam. Serial CellWrite is unchanged.
use super::*;
use crate::tagged::gc::SymbolWriteToken;

impl LispSymbol {
    pub(crate) fn parallel_store_plain(&self, value: Value, _: &SymbolWriteToken<'_>) -> bool {
        if !self.flags.is_plain_untrapped_unprojected() {
            return false;
        }
        self.val.store_release(value.bits());
        true
    }

    /// VOID materialization keeps the Forwarded tag. Only the descriptor
    /// identity changes; no shared VOID storage is ever written.
    pub(crate) fn parallel_replace_projection(
        &self,
        previous: FwdRef,
        next: FwdRef,
        _: &SymbolWriteToken<'_>,
    ) -> bool {
        if !self.flags.runtime_projected()
            || self.trapped_write() != SymbolTrappedWrite::Untrapped
            || self.forwarded_descriptor().map(FwdRef::address) != Some(previous.address())
        {
            return false;
        }
        self.val.store_release(next.address());
        true
    }
}
