//! Cold Green owner admission. No dormant call-path hook is introduced.
//!
//! Mutable leaves stay in owner-local TLS. Shared source slots carry raw leaf
//! pointers, but every reader checks the existing global leaf epoch. Each
//! baton acquisition invalidates those slots before ANY dispatch. This serial
//! protocol adds no hot owner check and does not certify parallel mutators.

use crate::emacs_core::eval::{Context, SpecBinding};
use crate::emacs_core::intern::SymId;
use crate::emacs_core::value::Value;
use std::marker::PhantomData;
use std::num::{NonZeroU64, NonZeroUsize};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FunctionEpoch(u64);
static_assertions::assert_impl_all!(FunctionEpoch: Send, Sync);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct JitOwnerId(NonZeroU64);
static_assertions::assert_impl_all!(JitOwnerId: Send, Sync);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeapIdentity(NonZeroUsize);
static_assertions::assert_impl_all!(HeapIdentity: Send, Sync);

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(crate) enum AdmissionError {
    #[error("green JIT admission does not certify inline2 virtual frames")]
    InlineFrames,
    #[error("green JIT yield does not certify active unguarded inline arithmetic")]
    UnguardedInlineArithmetic,
    #[error("green JIT preparation requires a valid owner and heap identity")]
    InvalidIdentity,
    #[error("green JIT restoration requires the original owner and heap")]
    ReturningOwner,
}
static_assertions::assert_impl_all!(AdmissionError: Send, Sync);

#[cold]
pub(crate) fn certify_admission() -> Result<(), AdmissionError> {
    use super::compile::Inline2Mode;
    if !super::jit_runtime_enabled() {
        return Ok(());
    }
    match super::compile::jit_inline2_mode() {
        Inline2Mode::Off => {}
        Inline2Mode::Named | Inline2Mode::Closure | Inline2Mode::Hof | Inline2Mode::All => {
            return Err(AdmissionError::InlineFrames);
        }
    }
    // All current backends share the physical native-frame/root-stack ABI.
    // Background workers publish only code results; install stays on the
    // original Lisp owner, under its Baton (see bg::install).
    Ok(())
}

/// Captured sources stay on the original continuation, never in a message.
#[derive(Clone, Copy)]
struct NamedSource {
    symbol: SymId,
    function: Value,
    _confined: PhantomData<Rc<()>>,
}
static_assertions::assert_not_impl_any!(NamedSource: Send, Sync);
impl std::fmt::Debug for NamedSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamedSource")
            .field("symbol", &self.symbol)
            .field("function_bits", &self.function.bits())
            .finish()
    }
}

/// CURRENT-owner preparation copied into the immutable owner/heap publication.
/// On return, captured sources pass to this owner's existing named-function
/// pins BEFORE publication retirement. Repeated parks therefore keep an old
/// definition alive even after another owner changed its canonical cell.
pub(crate) struct ParkedJit {
    owner: JitOwnerId,
    heap: HeapIdentity,
    function_epoch: FunctionEpoch,
    sources: Vec<NamedSource>,
    roots: Vec<Value>,
    _confined: PhantomData<Rc<()>>,
}
static_assertions::assert_not_impl_any!(ParkedJit: Send, Sync);
impl std::fmt::Debug for ParkedJit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Metadata only: never dereference copied heap Values for diagnostics.
        formatter
            .debug_struct("ParkedJit")
            .field("owner", &self.owner)
            .field("heap", &self.heap)
            .field("function_epoch", &self.function_epoch)
            .field("source_count", &self.sources.len())
            .field("root_words", &self.roots.len())
            .finish()
    }
}

impl ParkedJit {
    #[cold]
    pub(crate) fn prepare(context: &Context) -> Result<Self, AdmissionError> {
        let owner = JitOwnerId(
            NonZeroU64::new(context.context_instance_id())
                .ok_or(AdmissionError::InvalidIdentity)?,
        );
        let heap = HeapIdentity(
            NonZeroUsize::new(context.tagged_heap.identity())
                .ok_or(AdmissionError::InvalidIdentity)?,
        );
        let frames: Vec<_> = context
            .specpdl
            .iter()
            .filter_map(|entry| match entry {
                SpecBinding::Backtrace { function, .. }
                | SpecBinding::Backtrace1 { function, .. }
                | SpecBinding::Backtrace2 { function, .. }
                | SpecBinding::BacktraceNative { function, .. } => Some(*function),
                SpecBinding::Let { .. }
                | SpecBinding::LetLocal { .. }
                | SpecBinding::LetDefault { .. }
                | SpecBinding::LexicalEnv { .. }
                | SpecBinding::GcRoot { .. }
                | SpecBinding::UnwindProtect { .. }
                | SpecBinding::SaveExcursion { .. }
                | SpecBinding::SaveCurrentBuffer { .. }
                | SpecBinding::SaveRestriction { .. }
                | SpecBinding::LoadsInProgress { .. }
                | SpecBinding::RequireStack { .. }
                | SpecBinding::NativeUnwind { .. }
                | SpecBinding::Nop => None,
            })
            .collect();
        let mut called: Vec<_> = frames
            .iter()
            .filter_map(|function| function.as_symbol_id(context.symbol_mode()))
            .collect();
        called.sort_unstable_by_key(|symbol| symbol.0);
        called.dedup();
        let sources: Vec<_> = called
            .iter()
            .filter_map(|&symbol| {
                context
                    .obarray
                    .indirect_function_id(symbol)
                    .map(|function| NamedSource {
                        symbol,
                        function,
                        _confined: PhantomData,
                    })
            })
            .collect();
        let mut roots: Vec<_> = sources.iter().map(|source| source.function).collect();
        roots.extend(frames.into_iter().filter(|function| function.is_bytecode()));
        super::cache::trace_redefined_pins(
            context.tagged_heap.identity(),
            &|symbol| called.binary_search_by_key(&symbol.0, |s| s.0).is_ok(),
            &mut |function| roots.push(function),
        );
        // Baseline bit-op inlining has no post-switch epoch guard, including
        // legacy OSR variants with no dependency registration. Reject that
        // active shape before publication or binding exchange. Guarded MIR
        // InlineEntry and physical calls retain their existing epoch checks.
        if super::compile::jit_inline_arith_on() {
            for function in &roots {
                if let Some(func) = function.bytecode_data_if_materialized()
                    && super::cache::green_source_has_native_state(func)
                    && !super::compile::inline_arith_callee_syms(
                        func.executable_ops(),
                        &func.constants,
                    )
                    .is_empty()
                {
                    return Err(AdmissionError::UnguardedInlineArithmetic);
                }
            }
        }
        Ok(Self {
            owner,
            heap,
            function_epoch: FunctionEpoch(context.obarray.function_epoch()),
            sources,
            roots,
            _confined: PhantomData,
        })
    }
    pub(crate) fn roots(&self) -> &[Value] {
        &self.roots
    }
    /// Only the ORIGINAL returning owner invokes this while publication lives.
    /// Retire dependents; never clear executable storage under a native frame.
    #[cold]
    pub(crate) fn resume(self, context: &Context) -> Result<(), AdmissionError> {
        if self.owner.0.get() != context.context_instance_id()
            || self.heap.0.get() != context.tagged_heap.identity()
        {
            return Err(AdmissionError::ReturningOwner);
        }
        begin_owner_turn();
        for source in self.sources {
            super::cache::pin_redefined_function(source.symbol, source.function);
        }
        if self.function_epoch != FunctionEpoch(context.obarray.function_epoch()) {
            super::cache::retire_green_inline_dependents();
        }
        Ok(())
    }
}

/// Runs after acquisition, including return from a worker's final exit (which
/// may arm shared slots without yielding). No parked owner is accessed.
#[cold]
pub(crate) fn begin_owner_turn() {
    super::cache::invalidate_green_leaf_slots();
}
