//! Cold function publication under the retained World's symbol word.
use super::*;
use crate::tagged::gc::{FunctionError, RegistrationError, SymbolWriteToken};
use std::sync::atomic::Ordering;

pub(crate) struct FunctionCommit {
    pub(crate) previous: super::super::Value,
    epoch: u64,
}

impl SymbolBacking {
    /// # Safety
    /// The matching serial admission holds WRITER, before owner registration.
    pub(crate) unsafe fn prepare_function(
        &self,
        id: SymId,
        _authority: &SymbolWriteToken<'_>,
    ) -> Result<super::super::FunctionCellSnapshot, FunctionError> {
        let obarray = unsafe { self.obarray.as_ref() };
        if obarray.generation() != self.generation || obarray.get_by_id(id).is_none() {
            return Err(RegistrationError::Publication.into());
        }
        let side = obarray
            .symbols
            .sides
            .get(Obarray::slot_index(id) >> 12)
            .ok_or(RegistrationError::Publication)?;
        side.try_prepare_stamps()
            .ok_or(RegistrationError::Capacity)?;
        Ok(obarray.function_cell_snapshot(id))
    }

    /// # Safety
    /// The matching live writer excludes every reader, marker and legacy view.
    pub(crate) unsafe fn plan_fset(
        &self,
        id: SymId,
        value: super::super::Value,
        _authority: &SymbolWriteToken<'_>,
    ) -> Result<Option<FunctionCommit>, FunctionError> {
        let obarray = unsafe { self.obarray.as_ref() };
        if obarray.generation() != self.generation {
            return Err(RegistrationError::Publication.into());
        }
        let symbol = obarray
            .get_by_id(id)
            .ok_or(RegistrationError::Publication)?;
        if !symbol.function_unbound && symbol.function.get().bits() == value.bits() {
            return Ok(None);
        }
        let epoch = obarray
            .function_epoch()
            .checked_add(1)
            .filter(|&epoch| epoch != u64::MAX)
            .ok_or(FunctionError::EpochExhausted)?;
        let side = obarray
            .symbols
            .sides
            .get(Obarray::slot_index(id) >> 12)
            .ok_or(RegistrationError::Publication)?;
        if !side.stamps_prepared() {
            return Err(RegistrationError::Publication.into());
        }
        Ok(Some(FunctionCommit {
            previous: symbol.function.get(),
            epoch,
        }))
    }

    /// # Safety
    /// WRITER spans plan, journal retention and this commit. No callback or
    /// reference escapes. Derive the exclusive metadata loan from the original
    /// UnsafeCell root, after all symbol readers and legacy loans have ended.
    pub(crate) unsafe fn commit_fset(
        &self,
        id: SymId,
        value: super::super::Value,
        commit: FunctionCommit,
        _authority: &SymbolWriteToken<'_>,
    ) -> Result<(), FunctionError> {
        let obarray = unsafe { &mut *self.obarray.as_ptr() };
        let index = Obarray::slot_index(id);
        if obarray.generation() != self.generation
            || obarray.function_epoch().checked_add(1) != Some(commit.epoch)
            || !obarray
                .symbols
                .sides
                .get(index >> 12)
                .is_some_and(|s| s.stamps_prepared())
        {
            return Err(RegistrationError::Poisoned.into());
        }
        let symbol = obarray
            .symbols
            .get_mut(index)
            .ok_or(RegistrationError::Poisoned)?;
        // Use the landed interior-mutable cell, never an atomic cast or a
        // serial setter (which would touch another owner's TLS and SATB).
        symbol.function.store_atomic(value);
        symbol.function_unbound = false;
        let published = obarray.symbols.sides.get(index >> 12).is_some_and(|side| {
            side.stamp_prepared(index & (super::super::OBARRAY_CHUNK - 1), commit.epoch)
        });
        if !published {
            return Err(RegistrationError::Poisoned.into());
        }
        obarray
            .dispatch_cells
            .function_epoch
            .store(commit.epoch, Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) unsafe fn function_facts_for_test(
        &self,
        id: SymId,
        _authority: &crate::tagged::gc::SymbolReadToken<'_>,
    ) -> Option<(u64, u64)> {
        let obarray = unsafe { self.obarray.as_ref() };
        let index = Obarray::slot_index(id);
        let side = obarray.symbols.sides.get(index >> 12)?;
        Some((
            obarray.function_epoch(),
            side.stamp(index & (super::super::OBARRAY_CHUNK - 1)),
        ))
    }
}
