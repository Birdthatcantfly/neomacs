//! Slot-to-slot Plain globals. No legacy TLS or raw-value public API.
use super::*;
use crate::emacs_core::value::Value;
use crate::tagged::gc::mark_word::MarkWord;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalError {
    Authority(RegistrationError),
    VoidVariable,
    #[cfg(test)]
    AwaitingB143,
}

impl From<RegistrationError> for GlobalError {
    fn from(error: RegistrationError) -> Self {
        Self::Authority(error)
    }
}

/// No constructor outside this module. The word reference is borrowed only
/// while the owning SymbolRef remains live; the token cannot cross threads.
pub(crate) struct SymbolReadToken<'lease> {
    _word: &'lease AtomicU64,
    _thread: PhantomData<Rc<()>>,
}

fn check_home(run: &RunAccess<'_, '_>, home: &OwnerRootSlot<'_>) -> Result<(), RegistrationError> {
    let local = run
        .owner
        .local
        .as_ref()
        .ok_or(RegistrationError::StaleRegistration)?;
    home.check_binding(
        run.owner.entry.key,
        local.root_slot_generation,
        local.root_scratch.len(),
    )
}

impl<'era> RunAccess<'_, 'era> {
    /// A void error leaves dst unchanged. The loaded value becomes a root
    /// before the counted-reader lease is released, including heap pointers.
    pub fn sym_value_into(
        &mut self,
        symbol: &SymbolHandle,
        dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), GlobalError> {
        check_home(self, dst)?;
        self.symbol_ref(symbol)?.copy_into(dst.index, false)
    }

    pub fn sym_boundp_into(
        &mut self,
        symbol: &SymbolHandle,
        dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), GlobalError> {
        check_home(self, dst)?;
        self.symbol_ref(symbol)?.copy_into(dst.index, true)
    }
}

impl SymbolRef<'_, '_, '_> {
    fn copy_into(&mut self, index: usize, boundp: bool) -> Result<(), GlobalError> {
        let registry = self.run.owner.registry;
        // Obtain the destination before loading: no fallible work or poll is
        // permitted between the load and root installation.
        let dst = self
            .run
            .owner
            .local
            .as_mut()
            .and_then(|local| local.root_scratch.get_mut(index))
            .ok_or(RegistrationError::StaleRegistration)?;
        let authority = SymbolReadToken {
            _word: &registry.symbols.word,
            _thread: PhantomData,
        };
        // SAFETY: this lease validates identity and holds the real reader word;
        // World/backing is retained, and dst belongs to this resident owner.
        let value = unsafe {
            registry
                .symbols
                .backing
                .read_plain(self.key.symbol, &authority)
        }
        .ok_or(RegistrationError::Publication)?;
        let answer = if boundp {
            Value::bool_val(!value.is_unbound())
        } else if value.is_unbound() {
            return Err(GlobalError::VoidVariable);
        } else {
            value
        };
        *dst = MarkWord::of(answer);
        Ok(())
    }
}

static_assertions::assert_not_impl_any!(SymbolReadToken<'static>: Send, Sync, Clone, Copy);
