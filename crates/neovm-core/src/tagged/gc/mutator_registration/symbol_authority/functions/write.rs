//! Prepared, bounded function writes and stopped serial-cache reconciliation.
use super::*;
use crate::emacs_core::symbol::parallel::FunctionUpdate;
use std::cell::UnsafeCell;

struct FunctionRecord {
    symbol: SymId,
    lambda: SymId,
    previous: Option<MarkWord>,
}

pub(in crate::tagged::gc::mutator_registration) struct FunctionState {
    records: UnsafeCell<Vec<FunctionRecord>>,
    dirty: AtomicBool,
}
// SAFETY: records are prepared only before registration under WRITER. Running
// writes require that same word; stopped visits require the matching keeper
// minted with word zero. Readers do not access records. Only encoded mark
// words and symbol identities cross threads, never a Value or manager view.
unsafe impl Sync for FunctionState {}
impl FunctionState {
    pub(in crate::tagged::gc::mutator_registration) fn new() -> Self {
        Self {
            records: UnsafeCell::new(Vec::new()),
            dirty: AtomicBool::new(false),
        }
    }
    pub(in crate::tagged::gc::mutator_registration) fn finish_ready(
        &self,
    ) -> Result<(), RegistrationError> {
        if self.dirty.load(Ordering::Acquire) {
            Err(RegistrationError::PendingRoots)
        } else {
            Ok(())
        }
    }
}

/// The initial closed call subset: no callbacks, mutable captures, alias
/// resolution or internal UNBOUND. The shared graph is immutable in this era.
fn closed_definition(value: Value, lambda: SymId) -> bool {
    if value.is_nil() {
        return true;
    }
    if !value.is_cons() || value.cons_car_unobserved().as_bare_symbol_id() != Some(lambda) {
        return false;
    }
    let args = value.cons_cdr_unobserved();
    if !args.is_cons() || !args.cons_car_unobserved().is_nil() {
        return false;
    }
    let body = args.cons_cdr_unobserved();
    if !body.is_cons() || !body.cons_cdr_unobserved().is_nil() {
        return false;
    }
    let answer = body.cons_car_unobserved();
    answer.is_nil() || answer.is_t() || answer.is_fixnum()
}

fn keeper_compiler_quiescent() -> bool {
    #[cfg(feature = "jit")]
    {
        return crate::emacs_core::jit::cache::native_depth() == 0
            && crate::emacs_core::jit::bg::pending_count() == 0;
    }
    #[cfg(not(feature = "jit"))]
    true
}

impl ParallelHeapEra<'_> {
    /// Reserve this existing symbol's stamp and one original-target journal
    /// slot before ANY owner registers. No running write grows Rust metadata.
    pub fn prepare_function_symbol(&self, key: &SymbolHandle) -> Result<(), FunctionError> {
        let registry = self.registry();
        if registry.keeper_thread != std::thread::current().id() {
            return Err(RegistrationError::WrongThread.into());
        }
        if !self
            .legacy_roots
            .as_ref()
            .is_some_and(FrozenLegacyRoots::permits_function_writes)
            || !keeper_compiler_quiescent()
        {
            return Err(RegistrationError::Publication.into());
        }
        {
            let gate = registry.lock()?;
            if gate.entries.len() != 1 || gate.rendezvous.phase != Phase::Idle {
                return Err(RegistrationError::Busy.into());
            }
        }
        let symbols = &registry.symbols;
        symbols.validate(key, registry)?;
        if !word::writer_enter(&symbols.word) {
            return Err(RegistrationError::Busy.into());
        }
        let guard = AdmissionWriter(symbols);
        let authority = guard.write_token();
        let lambda = crate::emacs_core::intern::lookup_interned("lambda")
            .ok_or(FunctionError::UnsupportedDefinition)?;
        // SAFETY: retained backing, identity checked and exclusive admission.
        let initial = unsafe { symbols.backing.prepare_function(key.symbol, &authority) }?;
        if let FunctionCellSnapshot::Bound(value) = initial {
            if !closed_definition(value, lambda) {
                return Err(FunctionError::UnsupportedDefinition);
            }
        }
        // SAFETY: only this keeper can register an owner, and WRITER is held.
        let records = unsafe { &mut *symbols.functions.records.get() };
        if !records.iter().any(|r| r.symbol == key.symbol) {
            records
                .try_reserve(1)
                .map_err(|_| RegistrationError::Capacity)?;
            records.push(FunctionRecord {
                symbol: key.symbol,
                lambda,
                previous: None,
            });
        }
        Ok(())
    }
}

impl<'era> RunAccess<'_, 'era> {
    /// Both operands stay in resident root homes. A no-op consumes no epoch,
    /// stamp, journal slot or allocation; every refusal preserves the answer.
    pub fn sym_fset_into(
        &mut self,
        symbol: &SymbolHandle,
        src: &OwnerRootSlot<'era>,
        dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), FunctionError> {
        globals::check_home(self, src)?;
        globals::check_home(self, dst)?;
        let value = self
            .owner
            .local
            .as_ref()
            .and_then(|local| local.root_scratch.get(src.index))
            .ok_or(RegistrationError::StaleRegistration)?
            .value();
        self.symbol_mut(symbol)?
            .write_function_into(dst.index, FunctionUpdate::Set(value))
    }

    /// Store the explicit-unbound state, returning the original symbol even
    /// for a repeated no-op. Every validation precedes mutation of the answer.
    pub fn sym_fmakunbound_into(
        &mut self,
        symbol: &SymbolHandle,
        dst: &mut OwnerRootSlot<'era>,
    ) -> Result<(), FunctionError> {
        globals::check_home(self, dst)?;
        self.symbol_mut(symbol)?
            .write_function_into(dst.index, FunctionUpdate::Unbind)
    }
}

pub(in crate::tagged::gc::mutator_registration::symbol_authority) fn publish(
    symbols: &SymbolSidecar,
    id: SymId,
    update: FunctionUpdate,
    dst: &mut MarkWord,
    authority: &SymbolWriteToken<'_>,
) -> Result<(), FunctionError> {
    // SAFETY: private token comes from the matching identity-checked writer;
    // no running reader touches this vector, and it never grows here.
    let records = unsafe { &mut *symbols.functions.records.get() };
    let record = records
        .iter_mut()
        .find(|r| r.symbol == id)
        .ok_or(RegistrationError::Publication)?;
    let answer = match update {
        FunctionUpdate::Set(value) => {
            if !closed_definition(value, record.lambda) {
                return Err(FunctionError::UnsupportedDefinition);
            }
            value
        }
        FunctionUpdate::Unbind => Value::from_sym_id(id),
    };
    // SAFETY: retained World and exclusive word, no callback or structural change.
    let planned = unsafe { symbols.backing.plan_function_write(id, update, authority) }?;
    if let Some(commit) = planned {
        if record.previous.is_none() {
            record.previous = Some(MarkWord::of(commit.previous));
        }
        symbols.functions.dirty.store(true, Ordering::Release);
        // Original legacy target is now a stopped trace obligation. Every
        // remaining store is allocation-free; unexpected refusal is terminal.
        if let Err(error) = unsafe {
            symbols
                .backing
                .commit_function_write(id, update, commit, authority)
        } {
            symbols.poison();
            return Err(error);
        }
    }
    *dst = MarkWord::of(answer);
    Ok(())
}

impl KeeperSymbols<'_> {
    pub(in crate::tagged::gc::mutator_registration) fn append_function_roots(
        &self,
        roots: &mut Vec<Value>,
    ) -> Result<(), RegistrationError> {
        self.registry.symbols.ready()?;
        // SAFETY: the matching AllStopped remains exclusively borrowed; no
        // writer can modify journal entries during this stopped trace.
        let records = unsafe { &*self.registry.symbols.functions.records.get() };
        roots.extend(
            records
                .iter()
                .filter_map(|r| r.previous)
                .map(MarkWord::value),
        );
        Ok(())
    }

    /// The retained serial keeper owns these TLS caches. Original admission
    /// retirement occurs in its current returning Context receiver, before
    /// serial execution; no header pointer can outlive that receiver's loan.
    pub fn reconcile_functions(&mut self) -> Result<(), FunctionError> {
        self.registry.symbols.ready()?;
        if !keeper_compiler_quiescent() {
            return Err(RegistrationError::Publication.into());
        }
        struct PoisonOnUnwind<'a> {
            registry: &'a Registry,
            armed: bool,
        }
        impl Drop for PoisonOnUnwind<'_> {
            fn drop(&mut self) {
                if self.armed {
                    self.registry.symbols.poison();
                    self.registry.poison();
                }
            }
        }
        let mut guard = PoisonOnUnwind {
            registry: self.registry,
            armed: true,
        };
        // SAFETY: exclusive matching AllStopped, keeper thread, zero word.
        let state = &self.registry.symbols.functions;
        let records = unsafe { &mut *state.records.get() };
        for record in records.iter().filter(|r| r.previous.is_some()) {
            #[cfg(feature = "jit")]
            crate::emacs_core::jit::cache::evict_inline_dependents(record.symbol);
            crate::emacs_core::eval::note_function_epoch_move(
                crate::emacs_core::symbol::FunctionEpochBump::InternalCellWrite,
                Some(record.symbol),
            );
        }
        for record in records {
            record.previous = None;
        }
        state.dirty.store(false, Ordering::Release);
        guard.armed = false;
        Ok(())
    }
}

#[cfg(test)]
impl RunAccess<'_, '_> {
    pub(crate) fn function_facts_for_test(
        &mut self,
        key: &SymbolHandle,
    ) -> Result<(u64, u64, bool), FunctionError> {
        let lease = self.symbol_ref(key)?;
        let token = lease.read_token();
        let symbols = &lease.run.owner.registry.symbols;
        let (epoch, stamp) = unsafe { symbols.backing.function_facts_for_test(key.symbol, &token) }
            .ok_or(RegistrationError::Publication)?;
        Ok((
            epoch,
            stamp,
            symbols.functions.dirty.load(Ordering::Acquire),
        ))
    }
}
