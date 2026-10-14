//! Owner-local dynamic bindings (Phase 0 step 2, round 3).
//!
//! Once a variable is projected (`forward::thread_projection`), a dynamic
//! binding of it no longer writes shared storage. It lives in the binding
//! OWNER's slab: a slot per projected variable and channel holding the
//! owner's current binding, and a binding stack of records saying what each
//! binding hides. The specpdl carries one `NativeUnwindAction::OwnerBinding`
//! per binding, in GNU's order, so unwinding, `backtrace-eval` and panic
//! recovery reach the record exactly where they would reach a `Let` entry.
//!
//! Reads and writes consult the owner's slot first and the variable's
//! canonical storage otherwise (`eval::thread_projection`). The canonical
//! storage is what every other owner sees: an owner's binding is invisible
//! to them, and a write by an owner with no binding is visible to all, as
//! in GNU's threads, where a thread switch swaps the bindings out
//! (`src/thread.c:87-99`).
//!
//! Slabs are allocated by the World and registered there; the collector
//! traces every registered slab in its root phase, so a parked owner's
//! bindings stay rooted without reading its Context. A slot write first
//! notes the value it overwrites for a running mark (SATB).
//!
//! No production path creates an owner yet: a slab exists only once a
//! projected variable is bound, and only tests project variables.

use super::{Context, NativeUnwindAction, SpecBinding};
use crate::buffer::{BufferId, BufferManager};
use crate::emacs_core::error::Flow;
use crate::emacs_core::forward::{
    CanonicalArm, LispBoolFwd, LispBufferObjFwd, ThreadProjectionDescriptor, TlsIndex,
};
use crate::emacs_core::intern::SymId;
use crate::emacs_core::symbol::{CanonicalAccess, SetInternalBind};
use crate::emacs_core::value::Value;
use rustc_hash::FxHashMap;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ptr::NonNull;

/// Slots per page of an owner's default-channel table.
const SLOT_PAGE: usize = 256;

/// The value of an owner's binding. Any value, including
/// [`Value::UNBOUND`] for a binding made void (`makunbound` inside a `let`).
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub(crate) struct SlotValue(Value);

impl SlotValue {
    #[inline]
    pub(crate) fn new(value: Value) -> Self {
        Self(value)
    }

    /// The stored value; [`Value::UNBOUND`] for a void binding.
    #[inline]
    pub(crate) fn get(self) -> Value {
        self.0
    }

    /// The value, or `None` for a void binding.
    #[inline]
    pub(crate) fn bound(self) -> Option<Value> {
        (!self.0.is_unbound()).then_some(self.0)
    }
}

/// Whether this owner binds a variable on one channel. `NoBinding` is not a
/// value: reads then fall through to canonical storage.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum SlotState {
    #[default]
    NoBinding,
    Bound(SlotValue),
}

impl SlotState {
    #[inline]
    pub(crate) fn value(self) -> Option<SlotValue> {
        match self {
            Self::NoBinding => None,
            Self::Bound(value) => Some(value),
        }
    }
}

/// Which storage of a variable a binding shadows: its default (global, or
/// the default of a buffer-local variable) or one buffer's local binding.
/// A buffer id is never reused, so it names that buffer for good.
///
/// Neomacs has one keyboard, so a `DEFVAR_KBOARD` variable binds on the
/// default channel; several terminals would add a keyboard channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Channel {
    Default,
    BufferLocal(BufferId),
}

impl Channel {
    /// The buffer an assignment on this channel lands in, as
    /// `store_runtime_binding` reports it.
    #[inline]
    pub(crate) fn locus(self) -> Option<BufferId> {
        match self {
            Self::Default => None,
            Self::BufferLocal(buffer) => Some(buffer),
        }
    }
}

/// GNU's kind of `let` entry for the binding (`src/lisp.h:3548-3567`): what
/// `default-toplevel-value` and `let_shadows_buffer_binding_p` look for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingKind {
    /// `SPECPDL_LET`.
    Let,
    /// `SPECPDL_LET_DEFAULT`, recording the buffer current at the bind.
    LetDefault { where_buf: Option<BufferId> },
    /// `SPECPDL_LET_LOCAL`: the channel names the buffer.
    LetLocal,
}

/// What popping a binding restores.
#[derive(Clone, Copy, Debug)]
pub(crate) enum UndoTarget {
    /// The binding hides no binding of this owner: popping it exposes the
    /// canonical value, whatever another owner wrote meanwhile. SUSPENDED
    /// holds the binding while `backtrace-eval` has unwound it.
    Inherited { suspended: Option<SlotValue> },
    /// The binding hides this owner's outer binding with this value.
    Previous(SlotValue),
    /// The canonical storage the binding hid was killed by this owner
    /// (`kill-local-variable`) while bound; this is the value it held, which
    /// GNU's specpdl entry still carries and restores if the local is made
    /// again.
    Captured(SlotValue),
    /// The binding was dropped, as GNU turns a `let` whose local is gone
    /// into `SPECPDL_NOP` (`src/eval.c:4204-4228`); popping it does nothing.
    Retired,
}

impl UndoTarget {
    fn values(self) -> [Option<SlotValue>; 1] {
        match self {
            Self::Inherited { suspended } => [suspended],
            Self::Previous(value) | Self::Captured(value) => [Some(value)],
            Self::Retired => [None],
        }
    }
}

/// One binding on an owner's binding stack.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnerBindingRecord {
    pub(crate) sym: SymId,
    pub(crate) index: TlsIndex,
    pub(crate) kind: BindingKind,
    pub(crate) channel: Channel,
    pub(crate) undo: UndoTarget,
    generation: NonZeroU32,
}

/// A record's position on its owner's binding stack plus the generation it
/// was issued at, so a stale id (a record already popped, its position
/// reused) never names the new record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnerRecordId {
    depth: NonZeroU32,
    generation: NonZeroU32,
}

impl OwnerRecordId {
    #[inline]
    fn position(self) -> usize {
        // A depth fits `usize` on every supported target.
        (self.depth.get() - 1) as usize
    }
}

/// The specpdl's handle on an owner binding: the bound symbol, read by the
/// unwinder to decide whether watchers make the pop a Lisp cleanup, and the
/// record. No `Value`: the record's values live in the slab, which the
/// collector traces.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnerBindingRef {
    sym: SymId,
    record: OwnerRecordId,
}

impl OwnerBindingRef {
    /// The symbol the binding binds (alias-resolved, as GNU records it).
    #[inline]
    pub(crate) fn symbol(self) -> SymId {
        self.sym
    }
}

/// One owner's bindings: the slots holding its current binding per variable
/// and channel, and its binding stack.
///
/// Owned by the World's [`OwnerSlabRegistry`], mutated only by its owner,
/// and read by the collector's root phase.
pub(crate) struct OwnerSlab {
    /// The heap whose values the slab holds.
    heap: usize,
    /// Default-channel slots, by `TlsIndex::slot()`, in lazily made pages.
    pages: Vec<Option<Box<[SlotState; SLOT_PAGE]>>>,
    /// Buffer-local-channel slots that hold a binding.
    locals: FxHashMap<(TlsIndex, BufferId), SlotValue>,
    /// The binding stack, in specpdl order; a popped record leaves `None`
    /// until every record above it is popped too.
    records: Vec<Option<OwnerBindingRecord>>,
    next_generation: NonZeroU32,
    /// `debug-on-next-call`'s slot index and the owner's own cell for it,
    /// once the owner binds it: the cell mirrors the default-channel slot,
    /// so compiled code's armed test (one load through the admission
    /// header) reads the owner's binding.
    debugger: Option<(TlsIndex, &'static LispBoolFwd)>,
    /// The values of the owner's in-place buffer bindings while it is
    /// parked: its specpdl saves them then, and the slab keeps them traced.
    parked: Vec<Value>,
    _thread_confined: PhantomData<*const ()>,
}

static_assertions::assert_not_impl_any!(OwnerSlab: Send, Sync, Clone);
static_assertions::assert_not_impl_any!(SlotValue: Send, Sync);

impl std::fmt::Debug for OwnerSlab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerSlab")
            .field("heap", &format_args!("{:#x}", self.heap))
            .field("records", &self.records.len())
            .field("locals", &self.locals.len())
            .finish_non_exhaustive()
    }
}

/// What a `DEFVAR_BOOL` cell holds for a binding STATE: GNU's `!NILP`.
#[inline]
fn debugger_armed(state: SlotState) -> bool {
    state
        .value()
        .and_then(SlotValue::bound)
        .is_some_and(|value| !value.is_nil())
}

/// Note a value leaving the slab for a running mark: the snapshot taken at
/// the mark's start may name it, and nothing else need hold it now.
#[inline]
fn note_dropped(value: Option<SlotValue>) {
    if let Some(value) = value {
        crate::tagged::gc::note_root_overwrite(value.get());
    }
}

impl OwnerSlab {
    fn new(heap: usize) -> Self {
        Self {
            heap,
            pages: Vec::new(),
            locals: FxHashMap::default(),
            records: Vec::new(),
            next_generation: NonZeroU32::MIN,
            debugger: None,
            parked: Vec::new(),
            _thread_confined: PhantomData,
        }
    }

    /// The slot of the variable INDEX names on CHANNEL.
    #[inline]
    pub(crate) fn slot(&self, index: TlsIndex, channel: Channel) -> SlotState {
        match channel {
            Channel::Default => {
                let slot = index.slot();
                self.pages
                    .get(slot / SLOT_PAGE)
                    .and_then(Option::as_deref)
                    .map_or(SlotState::NoBinding, |page| page[slot % SLOT_PAGE])
            }
            Channel::BufferLocal(buffer) => self
                .locals
                .get(&(index, buffer))
                .map_or(SlotState::NoBinding, |value| SlotState::Bound(*value)),
        }
    }

    /// Replace the slot of INDEX on CHANNEL, noting the value it held.
    #[inline]
    pub(crate) fn set_slot(&mut self, index: TlsIndex, channel: Channel, state: SlotState) {
        let old = match channel {
            Channel::Default => {
                let slot = index.slot();
                match self
                    .pages
                    .get_mut(slot / SLOT_PAGE)
                    .and_then(Option::as_deref_mut)
                {
                    Some(page) => std::mem::replace(&mut page[slot % SLOT_PAGE], state).value(),
                    None if matches!(state, SlotState::NoBinding) => None,
                    None => {
                        self.new_page(slot)[slot % SLOT_PAGE] = state;
                        None
                    }
                }
            }
            Channel::BufferLocal(buffer) => match state {
                SlotState::Bound(value) => self.locals.insert((index, buffer), value),
                SlotState::NoBinding => self.locals.remove(&(index, buffer)),
            },
        };
        note_dropped(old);
        if let Some((debugger, cell)) = self.debugger
            && debugger == index
            && channel == Channel::Default
        {
            cell.set(debugger_armed(state));
        }
    }

    /// Keep VALUES traced while the owner is parked.
    pub(crate) fn park(&mut self, values: Vec<Value>) {
        debug_assert!(self.parked.is_empty(), "an owner parks once");
        self.parked = values;
    }

    /// The owner runs again: its parked values are back in its specpdl.
    pub(crate) fn unpark(&mut self) {
        for value in std::mem::take(&mut self.parked) {
            note_dropped(Some(SlotValue::new(value)));
        }
    }

    /// This owner's own `debug-on-next-call` cell for the variable INDEX
    /// names, made on first use and holding the default-channel slot.
    pub(crate) fn debugger_cell(&mut self, index: TlsIndex) -> &'static LispBoolFwd {
        match self.debugger {
            Some((debugger, cell)) => {
                debug_assert_eq!(debugger, index, "one debug-on-next-call per owner");
                cell
            }
            None => {
                let armed = debugger_armed(self.slot(index, Channel::Default));
                let cell = crate::emacs_core::forward::alloc_owner_debugger_cell(armed);
                self.debugger = Some((index, cell));
                cell
            }
        }
    }

    /// The page holding default-channel SLOT, made on first use.
    #[cold]
    #[inline(never)]
    fn new_page(&mut self, slot: usize) -> &mut [SlotState; SLOT_PAGE] {
        let page = slot / SLOT_PAGE;
        if page >= self.pages.len() {
            self.pages.resize_with(page + 1, || None);
        }
        self.pages[page].get_or_insert_with(|| Box::new([SlotState::NoBinding; SLOT_PAGE]))
    }

    /// Push a record and return its id.
    pub(crate) fn push_record(
        &mut self,
        sym: SymId,
        index: TlsIndex,
        kind: BindingKind,
        channel: Channel,
        undo: UndoTarget,
    ) -> OwnerRecordId {
        let generation = self.next_generation;
        self.next_generation = generation.checked_add(1).unwrap_or(NonZeroU32::MIN);
        self.records.push(Some(OwnerBindingRecord {
            sym,
            index,
            kind,
            channel,
            undo,
            generation,
        }));
        let depth = u32::try_from(self.records.len())
            .ok()
            .and_then(NonZeroU32::new)
            .expect("an owner binding stack deeper than u32");
        OwnerRecordId { depth, generation }
    }

    /// The live record ID names, if any.
    #[inline]
    pub(crate) fn record(&self, id: OwnerRecordId) -> Option<&OwnerBindingRecord> {
        self.records
            .get(id.position())?
            .as_ref()
            .filter(|record| record.generation == id.generation)
    }

    /// Replace the undo target of the live record ID names, noting the
    /// value it held. Returns the previous target.
    pub(crate) fn replace_undo(
        &mut self,
        id: OwnerRecordId,
        undo: UndoTarget,
    ) -> Option<UndoTarget> {
        let record = self
            .records
            .get_mut(id.position())?
            .as_mut()
            .filter(|record| record.generation == id.generation)?;
        let old = std::mem::replace(&mut record.undo, undo);
        let [value] = old.values();
        note_dropped(value);
        Some(old)
    }

    /// Take the record ID names off the stack: `None` if it is already gone.
    /// The caller owns the record's values from here on.
    #[inline]
    pub(crate) fn take_record(&mut self, id: OwnerRecordId) -> Option<OwnerBindingRecord> {
        if id.position() + 1 != self.records.len() {
            return self.take_record_below(id);
        }
        if self.records.last()?.as_ref()?.generation != id.generation {
            return None;
        }
        let record = self.records.pop()??;
        if self.records.last().is_some_and(Option::is_none) {
            self.trim_taken();
        }
        Some(record)
    }

    /// [`Self::take_record`] of a record with live records above it.
    #[cold]
    #[inline(never)]
    fn take_record_below(&mut self, id: OwnerRecordId) -> Option<OwnerBindingRecord> {
        let slot = self.records.get_mut(id.position())?;
        if slot.as_ref()?.generation != id.generation {
            return None;
        }
        slot.take()
    }

    #[cold]
    #[inline(never)]
    fn trim_taken(&mut self) {
        while self.records.last().is_some_and(Option::is_none) {
            self.records.pop();
        }
    }

    /// The live records binding SYM, outermost first.
    pub(crate) fn records_of(&self, sym: SymId) -> impl Iterator<Item = &OwnerBindingRecord> {
        self.records
            .iter()
            .flatten()
            .filter(move |record| record.sym == sym)
    }

    /// The live records binding SYM, with their ids, outermost first.
    fn record_ids_of(&self, sym: SymId) -> Vec<OwnerRecordId> {
        self.live_records()
            .filter(|(_, record)| record.sym == sym)
            .map(|(id, _)| id)
            .collect()
    }

    /// Every live record with its id, outermost first.
    pub(crate) fn live_records(
        &self,
    ) -> impl Iterator<Item = (OwnerRecordId, &OwnerBindingRecord)> {
        self.records
            .iter()
            .enumerate()
            .filter_map(|(position, record)| {
                let record = record.as_ref()?;
                let depth = u32::try_from(position + 1).ok().and_then(NonZeroU32::new)?;
                Some((
                    OwnerRecordId {
                        depth,
                        generation: record.generation,
                    },
                    record,
                ))
            })
    }

    /// Whether this owner binds SYM at all (GNU's `kind >= SPECPDL_LET` scan
    /// for SYM, `src/eval.c:706-710`).
    pub(crate) fn binds(&self, sym: SymId) -> bool {
        self.records_of(sym).next().is_some()
    }

    /// GNU `let_shadows_buffer_binding_p` (`src/eval.c:3593-3610`): a
    /// default binding of SYM made with BUFFER current.
    pub(crate) fn shadows_buffer_binding(&self, sym: SymId, buffer: Option<BufferId>) -> bool {
        buffer.is_some()
            && self
                .records_of(sym)
                .any(|record| record.kind == BindingKind::LetDefault { where_buf: buffer })
    }

    /// Visit every value the slab holds: each record's saved value and the
    /// current value of the channel it binds. Every bound slot belongs to a
    /// live record, so this visits each bound slot.
    pub(crate) fn trace_roots(&self, visit: &mut dyn FnMut(Value)) {
        for value in &self.parked {
            visit(*value);
        }
        for record in self.records.iter().flatten() {
            let [saved] = record.undo.values();
            if let Some(saved) = saved {
                visit(saved.get());
            }
            if let SlotState::Bound(current) = self.slot(record.index, record.channel) {
                visit(current.get());
            }
        }
    }
}

/// The World's owner slabs: allocation and the collector's root walk.
#[derive(Default)]
pub(crate) struct OwnerSlabRegistry {
    /// Allocations owned by this registry (`Box::into_raw`), freed on drop.
    slabs: Vec<NonNull<OwnerSlab>>,
    _thread_confined: PhantomData<*const ()>,
}

static_assertions::assert_not_impl_any!(OwnerSlabRegistry: Send, Sync, Clone);

impl std::fmt::Debug for OwnerSlabRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerSlabRegistry")
            .field("slabs", &self.slabs.len())
            .finish()
    }
}

impl OwnerSlabRegistry {
    /// Allocate and register a slab for a new owner on HEAP.
    pub(crate) fn register(&mut self, heap: usize) -> NonNull<OwnerSlab> {
        self.slabs.reserve(1);
        let slab = NonNull::from(Box::leak(Box::new(OwnerSlab::new(heap))));
        self.slabs.push(slab);
        slab
    }

    /// Visit every registered slab's values (the collector's root phase).
    pub(crate) fn trace_roots(&self, heap: usize, visit: &mut dyn FnMut(Value)) {
        for slab in &self.slabs {
            // SAFETY: the registry owns every registered allocation until it
            // drops. Its owner mutates a slab only through an exclusive
            // Context borrow, and the root phase runs on the owner's thread
            // with no such borrow live (`Context::trace_roots` takes `&self`).
            let slab = unsafe { slab.as_ref() };
            debug_assert_eq!(slab.heap, heap, "an owner slab traced for another heap");
            slab.trace_roots(visit);
        }
    }
}

impl Drop for OwnerSlabRegistry {
    fn drop(&mut self) {
        for slab in self.slabs.drain(..) {
            // SAFETY: each pointer came from `Box::leak` in `register` and is
            // dropped exactly once, here; the Contexts that named it are gone
            // (each holds a World lease, so the World outlives them).
            drop(unsafe { Box::from_raw(slab.as_ptr()) });
        }
    }
}

/// A Context's owner slab, or none while the Context has bound no projected
/// variable. Occupies the former reserved opcode-epoch word: same size,
/// alignment and (absent) niche, so no Context offset moves.
#[repr(transparent)]
pub(crate) struct OwnerSlot(Option<NonNull<OwnerSlab>>);

static_assertions::assert_eq_size!(OwnerSlot, std::cell::Cell<u64>);
static_assertions::assert_eq_align!(OwnerSlot, std::cell::Cell<u64>);
static_assertions::assert_not_impl_any!(OwnerSlot: Send, Sync, Clone);

impl std::fmt::Debug for OwnerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OwnerSlot").field(&self.0).finish()
    }
}

impl OwnerSlot {
    /// No owner bindings: every Context starts so.
    pub(crate) const DORMANT: Self = Self(None);

    /// Install SLAB, which the World's registry owns.
    pub(super) fn install(&mut self, slab: NonNull<OwnerSlab>) {
        debug_assert!(self.0.is_none(), "a Context owns one slab");
        self.0 = Some(slab);
    }

    /// Whether no slab is installed (the Context has bound no projected
    /// variable).
    #[inline(always)]
    pub(crate) fn is_dormant(&self) -> bool {
        self.0.is_none()
    }

    /// The slab, for reading.
    #[inline]
    pub(crate) fn slab(&self) -> Option<&OwnerSlab> {
        // SAFETY: the World's registry keeps the allocation alive for as long
        // as this Context holds its World lease, and only this owner mutates
        // it, through `slab_mut`, which a shared borrow of the slot excludes.
        self.0.map(|slab| unsafe { slab.as_ref() })
    }

    /// The slab, for this owner's writes.
    #[inline]
    pub(crate) fn slab_mut(&mut self) -> Option<&mut OwnerSlab> {
        // SAFETY: as in `slab`; the exclusive borrow of the slot is the
        // owner's exclusive access, and the registry reads the slab only in
        // the root phase, which no `&mut Context` borrow overlaps.
        self.0.map(|mut slab| unsafe { slab.as_mut() })
    }
}

/// Whether BUFFER holds a local binding of a per-buffer variable.
#[inline]
pub(super) fn per_buffer_local(buffer: &crate::buffer::Buffer, buf_fwd: &LispBufferObjFwd) -> bool {
    buf_fwd.local_flags_idx < 0
        || buf_fwd
            .buffer_slot()
            .is_some_and(|slot| buffer.slot_local_flag(slot))
}

/// GNU `specbind`'s choice of entry for the variable PROJECTION wraps
/// (`src/eval.c:3656-3715`), and the channel the binding shadows. The same
/// channel is the one a read or an assignment in the current buffer meets
/// (`find_symbol_value`, `set_internal`).
pub(crate) fn binding_shape(
    buffers: &BufferManager,
    id: SymId,
    projection: &ThreadProjectionDescriptor,
) -> (BindingKind, Channel) {
    let arm = projection.arm();
    if arm == CanonicalArm::Plain {
        return (BindingKind::Let, Channel::Default);
    }
    let current = buffers.current_buffer();
    match arm {
        CanonicalArm::Plain => (BindingKind::Let, Channel::Default),
        CanonicalArm::Localized(_) => match current {
            Some(buffer) if buffer.local_variable_binding_cell(id).is_some() => {
                (BindingKind::LetLocal, Channel::BufferLocal(buffer.id))
            }
            Some(buffer) => (
                BindingKind::LetDefault {
                    where_buf: Some(buffer.id),
                },
                Channel::Default,
            ),
            None => (BindingKind::Let, Channel::Default),
        },
        CanonicalArm::Forwarded(builtin) => match builtin.descriptor().as_buffer_obj_fwd() {
            Some(buf_fwd) => match current {
                Some(buffer) if per_buffer_local(buffer, buf_fwd) => {
                    (BindingKind::LetLocal, Channel::BufferLocal(buffer.id))
                }
                _ => (
                    BindingKind::LetDefault {
                        where_buf: current.map(|buffer| buffer.id),
                    },
                    Channel::Default,
                ),
            },
            None => (BindingKind::Let, Channel::Default),
        },
    }
}

/// The channel a read or assignment of the variable PROJECTION wraps meets
/// in the current buffer.
#[inline]
pub(crate) fn access_channel(
    buffers: &BufferManager,
    id: SymId,
    projection: &ThreadProjectionDescriptor,
) -> Channel {
    binding_shape(buffers, id, projection).1
}

/// Where an assignment (GNU `set_internal`, `src/data.c:1678-1831`) of the
/// variable PROJECTION wraps lands for this owner: the channel of the
/// owner's binding, or `None` for canonical storage.
///
/// A `set` of an automatically buffer-local variable with no local binding
/// in the current buffer makes one (canonical) unless a default binding of
/// this owner made in this buffer shadows it (`src/data.c:1756-1782`,
/// `:1810-1823`); a `let` or its unwind never makes one.
pub(crate) fn owner_store_channel(
    slab: &OwnerSlab,
    buffers: &BufferManager,
    id: SymId,
    projection: &ThreadProjectionDescriptor,
    bindflag: SetInternalBind,
) -> Option<Channel> {
    let channel = access_channel(buffers, id, projection);
    if channel == Channel::Default
        && bindflag == SetInternalBind::Set
        && set_makes_local(projection)
        && buffers.current_buffer_id().is_some()
        && !slab.shadows_buffer_binding(id, buffers.current_buffer_id())
    {
        return None;
    }
    slab.slot(projection.index(), channel)
        .value()
        .map(|_| channel)
}

/// Whether a `set` with no local binding makes one: a local-if-set variable
/// or a per-buffer variable with a local flag.
fn set_makes_local(projection: &ThreadProjectionDescriptor) -> bool {
    match projection.arm() {
        CanonicalArm::Plain => false,
        // SAFETY: a projection's localized arm names a live record of the
        // obarray's BLV pool; one flag is read.
        CanonicalArm::Localized(blv) => unsafe { (*blv.blv().as_ptr()).local_if_set },
        CanonicalArm::Forwarded(builtin) => builtin
            .descriptor()
            .as_buffer_obj_fwd()
            .is_some_and(|buf_fwd| buf_fwd.local_flags_idx >= 0),
    }
}

impl Context {
    /// This owner's slab, made and registered with the World on first use.
    pub(super) fn ensure_owner_slab(&mut self) -> &mut OwnerSlab {
        if self.owner.slab().is_none() {
            let slab = self.register_owner_slab();
            self.owner.install(slab);
        }
        self.owner
            .slab_mut()
            .expect("the owner slab was just installed")
    }

    /// GNU `specbind` (`src/eval.c:3656-3715`) of the projected variable
    /// RESOLVED: record the binding, run the `let` watchers, then store on
    /// the channel they left (`do_specbind` → `set_internal`). The binding
    /// is this owner's; canonical storage does not change.
    pub(super) fn specbind_owner(
        &mut self,
        resolved: SymId,
        projection: &'static ThreadProjectionDescriptor,
        value: Value,
    ) -> Result<(), Flow> {
        let (kind, channel) = binding_shape(&self.buffers, resolved, projection);
        // A buffer's own per-buffer slot binds in place (design §1.3b).
        if let (
            BindingKind::LetLocal,
            Channel::BufferLocal(buffer),
            CanonicalArm::Forwarded(builtin),
        ) = (kind, channel, projection.arm())
        {
            return self.specbind_slot_in_place(resolved, builtin.descriptor(), buffer, value);
        }
        let index = projection.index();
        let slab = self.ensure_owner_slab();
        let undo = match slab.slot(index, channel) {
            SlotState::Bound(outer) => UndoTarget::Previous(outer),
            SlotState::NoBinding => UndoTarget::Inherited { suspended: None },
        };
        let record = slab.push_record(resolved, index, kind, channel, undo);
        self.push_specpdl_with(|| SpecBinding::NativeUnwind {
            action: NativeUnwindAction::OwnerBinding(OwnerBindingRef {
                sym: resolved,
                record,
            }),
        });
        let watched = self.watchers.has_watchers(resolved);
        if watched {
            self.run_specbind_watcher(resolved, value, "let")?;
        }
        // A plain variable has no type rule (GNU's PLAINVAL store).
        let stored = match projection.logical_forwarder() {
            None => value,
            Some(_) => super::check_forwarded_store_at(
                &self.obarray,
                &self.buffers,
                &self.specpdl,
                resolved,
                value,
                super::ForwardStoreSite::Bind,
            )?
            .value(),
        };
        // A watcher may have localized the variable or bound another
        // channel: the value lands where GNU's `set_internal` would put it.
        if watched
            && !(self
                .obarray
                .thread_projection_of(resolved)
                .is_some_and(|now| std::ptr::eq(now, projection))
                && access_channel(&self.buffers, resolved, projection) == channel)
        {
            return self.set_internal_after_watchers(resolved, value, SetInternalBind::Bind);
        }
        if let Some(slab) = self.owner.slab_mut() {
            slab.set_slot(index, channel, SlotState::Bound(SlotValue::new(stored)));
        }
        self.publish_owner_binding(resolved);
        Ok(())
    }

    /// GNU `specbind`'s `SPECPDL_LET_LOCAL` of a per-buffer slot the current
    /// BUFFER holds (`src/eval.c:3641-3677`), for a projected variable: the
    /// binding lives in the slot, a shallow binding as GNU's, recorded by a
    /// `LetLocal` entry. A buffer's per-buffer slots always hold the
    /// effective value of the owner running in it, so every `BVAR` reader
    /// stays a slot load; an owner leaving the baton parks these bindings
    /// (design §1.3b).
    #[cold]
    #[inline(never)]
    fn specbind_slot_in_place(
        &mut self,
        resolved: SymId,
        logical: &'static crate::emacs_core::forward::LispFwd,
        buffer: BufferId,
        value: Value,
    ) -> Result<(), Flow> {
        let slot = logical
            .as_buffer_obj_fwd()
            .and_then(LispBufferObjFwd::buffer_slot)
            .expect("a per-buffer forwarder names a slot");
        let off = slot.index();
        let old_value = self
            .buffers
            .get(buffer)
            .map_or(Value::NIL, |buf| buf.slots[off]);
        self.push_specpdl_with(|| SpecBinding::LetLocal {
            sym_id: resolved,
            old_value,
            buffer_id: buffer,
        });
        self.run_specbind_watcher(resolved, value, "let")?;
        let stored = match logical.store(value) {
            Ok(store) => store.canonical_value(),
            Err(error) => return Err(super::forward_store_signal(error, value)),
        };
        if let Some(buf) = self.buffers.get_mut(buffer) {
            buf.slots[off] = stored;
        }
        Ok(())
    }

    /// Republish host mirrors of RESOLVED after an owner binding changed.
    #[inline]
    fn publish_owner_binding(&mut self, resolved: SymId) {
        if resolved == super::owner_mirrors::debug_on_next_call_symbol() {
            self.point_owner_debugger();
        }
        // The mask includes the GC settings, so an unmirrored variable needs
        // neither publication nor the GC-binding sync.
        if self.runtime_binding_has_projection(resolved) {
            let visible = match self.find_symbol_value_by_id(resolved) {
                Ok(super::SymbolValueLookup::Bound(value)) => value,
                _ => Value::NIL,
            };
            self.publish_runtime_binding_write_by_resolved_id(resolved, visible);
            self.sync_user_test_gc_binding_by_id(resolved);
        }
    }

    /// GNU `do_one_unbind` (`src/eval.c:3806-3885`) of an owner binding.
    ///
    /// A nested binding gives the owner's outer binding back. The outermost
    /// one leaves the owner unbound, so canonical storage shows through. With
    /// watchers, GNU notifies `unlet` while the binding is still in the shared
    /// cell and the entry already off the stack: the binding is visible to
    /// every thread until the restore, and a watcher that signals leaves it
    /// there. The binding is published to canonical storage for that window,
    /// then the restore runs through the paths a `Let` entry takes.
    #[cold]
    #[inline(never)]
    pub(super) fn unbind_owner_binding(&mut self, binding: OwnerBindingRef) -> Result<(), Flow> {
        let Some(record) = self
            .owner
            .slab_mut()
            .and_then(|slab| slab.take_record(binding.record))
        else {
            return Ok(());
        };
        let watched = self.watchers.has_watchers(record.sym);
        // The common case: an unwatched default binding is one slot store.
        if !watched && record.channel == Channel::Default {
            let state = match record.undo {
                UndoTarget::Previous(outer) => Some(SlotState::Bound(outer)),
                UndoTarget::Inherited { .. } => Some(SlotState::NoBinding),
                UndoTarget::Captured(_) | UndoTarget::Retired => None,
            };
            if let Some(state) = state
                && let Some(slab) = self.owner.slab_mut()
            {
                slab.set_slot(record.index, record.channel, state);
                self.publish_owner_binding(record.sym);
                return Ok(());
            }
        }
        let Some(projection) = self.obarray.thread_projection_of(record.sym) else {
            // `defvaralias` retired the projection; the alias resolves to its
            // base, which holds no binding of this owner.
            return self.unbind_retired_owner_record(record);
        };
        if !self.owner_channel_live(record.sym, projection, record.channel) {
            // GNU restores a buffer-local binding only while the buffer still
            // has the local (`Flocal_variable_p`, `src/eval.c:3871-3883`).
            note_dropped(record.undo.values()[0]);
            self.clear_owner_slot(record.index, record.channel);
            return Ok(());
        }
        match record.undo {
            UndoTarget::Retired => Ok(()),
            UndoTarget::Previous(outer) if !watched => {
                if let Some(slab) = self.owner.slab_mut() {
                    slab.set_slot(record.index, record.channel, SlotState::Bound(outer));
                }
                self.publish_owner_binding(record.sym);
                Ok(())
            }
            UndoTarget::Previous(outer) => {
                self.push_vm_frame_root(outer.get());
                self.restore_owner_binding(&record, outer.get())
            }
            UndoTarget::Inherited { .. } if !watched => {
                self.clear_owner_slot(record.index, record.channel);
                self.publish_owner_binding(record.sym);
                Ok(())
            }
            UndoTarget::Inherited { .. } => {
                let restore = self.owner_canonical_value(record.sym, record.channel);
                self.push_vm_frame_root(restore);
                self.publish_owner_slot(&record);
                self.restore_owner_binding(&record, restore)
            }
            UndoTarget::Captured(saved) if !watched => {
                self.clear_owner_slot(record.index, record.channel);
                self.owner_canonical_store(record.sym, record.channel, saved.get());
                self.publish_owner_binding(record.sym);
                Ok(())
            }
            UndoTarget::Captured(saved) => {
                self.push_vm_frame_root(saved.get());
                self.publish_owner_slot(&record);
                self.restore_owner_binding(&record, saved.get())
            }
        }
    }

    /// A record whose variable is no longer projected: restore what it saved
    /// through the variable's own paths.
    #[cold]
    fn unbind_retired_owner_record(&mut self, record: OwnerBindingRecord) -> Result<(), Flow> {
        match record.undo {
            UndoTarget::Previous(saved) | UndoTarget::Captured(saved) => {
                self.push_vm_frame_root(saved.get());
                self.set_internal_after_watchers(record.sym, saved.get(), SetInternalBind::Unbind)
            }
            UndoTarget::Inherited { .. } | UndoTarget::Retired => Ok(()),
        }
    }

    /// Move the owner's current binding of RECORD's channel into canonical
    /// storage and leave the owner unbound there: GNU's shared cell holding
    /// a popped binding while its `unlet` watchers run.
    fn publish_owner_slot(&mut self, record: &OwnerBindingRecord) {
        let current = self
            .owner
            .slab()
            .and_then(|slab| slab.slot(record.index, record.channel).value());
        if let Some(current) = current {
            self.push_vm_frame_root(current.get());
            self.owner_canonical_store(record.sym, record.channel, current.get());
        }
        self.clear_owner_slot(record.index, record.channel);
    }

    /// GNU's notify-then-store of VALUE for RECORD's kind of entry, through
    /// the same paths a `Let`, `LetDefault` or `LetLocal` pop takes; the
    /// stores land in this owner's slot when it still binds the channel.
    fn restore_owner_binding(
        &mut self,
        record: &OwnerBindingRecord,
        value: Value,
    ) -> Result<(), Flow> {
        let plain = self
            .obarray
            .thread_projection_of(record.sym)
            .is_some_and(|projection| projection.arm() == CanonicalArm::Plain);
        match (record.kind, record.channel) {
            (BindingKind::LetLocal, Channel::BufferLocal(buffer)) => {
                self.run_variable_watchers_by_id_with_where(
                    record.sym,
                    &value,
                    &Value::NIL,
                    "unlet",
                    &Value::make_buffer(buffer),
                )?;
                self.store_owner_channel(record, value);
                Ok(())
            }
            (BindingKind::Let, _) if plain => {
                let reported = if value.is_unbound() {
                    Value::NIL
                } else {
                    value
                };
                self.run_variable_watchers_by_id(record.sym, &reported, &Value::NIL, "unlet")?;
                self.set_internal_after_watchers(record.sym, value, SetInternalBind::Unbind)
            }
            _ => self.restore_default_binding_by_id(
                record.sym,
                SlotValue::new(value).bound(),
                SetInternalBind::Unbind,
            ),
        }
    }

    /// Store VALUE on RECORD's channel without watchers: the owner's slot
    /// while it binds the channel, canonical storage otherwise.
    fn store_owner_channel(&mut self, record: &OwnerBindingRecord, value: Value) {
        let bound = self
            .owner
            .slab()
            .is_some_and(|slab| slab.slot(record.index, record.channel).value().is_some());
        if bound {
            if let Some(slab) = self.owner.slab_mut() {
                slab.set_slot(
                    record.index,
                    record.channel,
                    SlotState::Bound(SlotValue::new(value)),
                );
            }
        } else {
            self.owner_canonical_store(record.sym, record.channel, value);
        }
        self.publish_owner_binding(record.sym);
    }

    pub(super) fn clear_owner_slot(&mut self, index: TlsIndex, channel: Channel) {
        if let Some(slab) = self.owner.slab_mut() {
            slab.set_slot(index, channel, SlotState::NoBinding);
        }
    }

    /// Whether CHANNEL still names storage of the variable: a buffer-local
    /// channel only while its buffer has the local.
    pub(super) fn owner_channel_live(
        &self,
        id: SymId,
        projection: &ThreadProjectionDescriptor,
        channel: Channel,
    ) -> bool {
        let Channel::BufferLocal(buffer) = channel else {
            return true;
        };
        let Some(buffer) = self.buffers.get(buffer) else {
            return false;
        };
        match projection.arm() {
            CanonicalArm::Plain => false,
            CanonicalArm::Localized(_) => buffer.local_variable_binding_cell(id).is_some(),
            CanonicalArm::Forwarded(builtin) => builtin
                .descriptor()
                .as_buffer_obj_fwd()
                .is_some_and(|buf_fwd| per_buffer_local(buffer, buf_fwd)),
        }
    }

    /// The canonical value of the projected variable ID on CHANNEL
    /// ([`Value::UNBOUND`] when void): what every owner without a binding
    /// there sees.
    pub(super) fn owner_canonical_value(&mut self, id: SymId, channel: Channel) -> Value {
        self.with_revealed_projection(id, |ctx| match channel {
            Channel::Default => super::super::data::default_value_by_id(ctx, id),
            Channel::BufferLocal(buffer) => ctx.local_binding_value_for_thread_switch(id, buffer),
        })
        .unwrap_or(Value::UNBOUND)
    }

    /// Store VALUE in the canonical storage of the projected variable ID on
    /// CHANNEL, below watchers and type signals (a value the storage refuses
    /// leaves it unchanged), as GNU's specpdl swaps do.
    pub(super) fn owner_canonical_store(&mut self, id: SymId, channel: Channel, value: Value) {
        self.with_revealed_projection(id, |ctx| match channel {
            Channel::Default => {
                ctx.discard_restore_default_binding(id, SlotValue::new(value).bound());
            }
            Channel::BufferLocal(buffer) => {
                if ctx
                    .local_binding_value_for_thread_switch(id, buffer)
                    .is_some()
                {
                    ctx.set_local_binding_for_thread_switch(id, buffer, value);
                }
            }
        });
    }

    /// Panic-recovery restore of an owner binding: storage only, no Lisp.
    #[cold]
    #[inline(never)]
    pub(super) fn discard_owner_binding(&mut self, binding: OwnerBindingRef) {
        let Some(record) = self
            .owner
            .slab_mut()
            .and_then(|slab| slab.take_record(binding.record))
        else {
            return;
        };
        let Some(projection) = self.obarray.thread_projection_of(record.sym) else {
            return;
        };
        if !self.owner_channel_live(record.sym, projection, record.channel) {
            note_dropped(record.undo.values()[0]);
            self.clear_owner_slot(record.index, record.channel);
            return;
        }
        match record.undo {
            UndoTarget::Retired => {}
            UndoTarget::Previous(outer) => {
                if let Some(slab) = self.owner.slab_mut() {
                    slab.set_slot(record.index, record.channel, SlotState::Bound(outer));
                }
            }
            UndoTarget::Inherited { .. } => self.clear_owner_slot(record.index, record.channel),
            UndoTarget::Captured(saved) => {
                self.clear_owner_slot(record.index, record.channel);
                self.owner_canonical_store(record.sym, record.channel, saved.get());
            }
        }
        self.publish_owner_binding(record.sym);
    }

    /// GNU `specpdl_unrewind`'s exchange for an owner binding
    /// (`src/eval.c:4177-4228`), owner-locally: the binding's value and the
    /// value it hides trade places, so `backtrace-eval` sees the frame's
    /// outer value and its rewind restores the binding. A binding that hid
    /// canonical storage is set aside (the owner reads canonical storage
    /// meanwhile) instead of being written there; a buffer-local binding
    /// whose local is gone is dropped, as GNU turns it into `SPECPDL_NOP`.
    #[cold]
    #[inline(never)]
    pub(super) fn swap_owner_binding(&mut self, binding: OwnerBindingRef) {
        let Some(record) = self
            .owner
            .slab()
            .and_then(|slab| slab.record(binding.record).copied())
        else {
            return;
        };
        let live = self
            .obarray
            .thread_projection_of(record.sym)
            .is_some_and(|projection| {
                self.owner_channel_live(record.sym, projection, record.channel)
            });
        let Some(slab) = self.owner.slab_mut() else {
            return;
        };
        if !live {
            slab.replace_undo(binding.record, UndoTarget::Retired);
            return;
        }
        let current = slab.slot(record.index, record.channel);
        let (undo, slot) = match (record.undo, current) {
            (UndoTarget::Previous(outer), SlotState::Bound(value)) => {
                (UndoTarget::Previous(value), SlotState::Bound(outer))
            }
            (UndoTarget::Inherited { suspended: None }, SlotState::Bound(value)) => (
                UndoTarget::Inherited {
                    suspended: Some(value),
                },
                SlotState::NoBinding,
            ),
            (
                UndoTarget::Inherited {
                    suspended: Some(value),
                },
                SlotState::NoBinding,
            ) => (
                UndoTarget::Inherited { suspended: None },
                SlotState::Bound(value),
            ),
            // A binding whose storage was killed, or a slot another binding
            // of this owner holds: nothing to exchange.
            _ => return,
        };
        slab.set_slot(record.index, record.channel, slot);
        slab.replace_undo(binding.record, undo);
        self.publish_owner_binding(record.sym);
    }

    /// GNU `Fdefault_toplevel_value`'s answer for an owner binding
    /// (`src/eval.c:811-825`): the value the outermost default binding of ID
    /// hides, `Some(None)` when void, or `None` when this owner has no
    /// default binding of ID.
    pub(crate) fn owner_default_toplevel_value(&mut self, id: SymId) -> Option<Option<Value>> {
        let outermost = self.owner.slab()?.records_of(id).find(|record| {
            matches!(
                record.kind,
                BindingKind::Let | BindingKind::LetDefault { .. }
            )
        })?;
        let value = match outermost.undo {
            UndoTarget::Previous(value) | UndoTarget::Captured(value) => value.get(),
            UndoTarget::Inherited { .. } | UndoTarget::Retired => {
                self.owner_canonical_value(id, outermost.channel)
            }
        };
        Some(SlotValue::new(value).bound())
    }

    /// GNU `Fset_default_toplevel_value` for an owner binding
    /// (`src/eval.c:827-840`): set the value the outermost default binding of
    /// ID hides. `false` when this owner has no default binding of ID.
    pub(crate) fn set_owner_default_toplevel_value(&mut self, id: SymId, value: Value) -> bool {
        let Some(slab) = self.owner.slab() else {
            return false;
        };
        let Some(outermost) = slab.record_ids_of(id).into_iter().find(|record| {
            slab.record(*record).is_some_and(|record| {
                matches!(
                    record.kind,
                    BindingKind::Let | BindingKind::LetDefault { .. }
                )
            })
        }) else {
            return false;
        };
        let Some(record) = slab.record(outermost).copied() else {
            return false;
        };
        match record.undo {
            UndoTarget::Previous(_) | UndoTarget::Captured(_) => {
                if let Some(slab) = self.owner.slab_mut() {
                    let undo = match record.undo {
                        UndoTarget::Captured(_) => UndoTarget::Captured(SlotValue::new(value)),
                        _ => UndoTarget::Previous(SlotValue::new(value)),
                    };
                    slab.replace_undo(outermost, undo);
                }
            }
            UndoTarget::Inherited { .. } | UndoTarget::Retired => {
                self.owner_canonical_store(id, record.channel, value);
            }
        }
        true
    }

    /// The same two questions for GNU's `local_toplevel_binding`
    /// (`src/eval.c:766-782`): the outermost buffer-local binding of ID in
    /// BUFFER. `None` when this owner has none.
    pub(crate) fn owner_local_toplevel_value(
        &mut self,
        id: SymId,
        buffer: BufferId,
    ) -> Option<Value> {
        let outermost = self
            .owner
            .slab()?
            .records_of(id)
            .find(|record| record.channel == Channel::BufferLocal(buffer))
            .copied()?;
        Some(match outermost.undo {
            UndoTarget::Previous(value) | UndoTarget::Captured(value) => value.get(),
            UndoTarget::Inherited { .. } | UndoTarget::Retired => {
                self.owner_canonical_value(id, outermost.channel)
            }
        })
    }

    /// Set the value the outermost buffer-local binding of ID in BUFFER
    /// hides. `false` when this owner has none.
    pub(crate) fn set_owner_local_toplevel_value(
        &mut self,
        id: SymId,
        buffer: BufferId,
        value: Value,
    ) -> bool {
        let Some(slab) = self.owner.slab() else {
            return false;
        };
        let Some(outermost) = slab.record_ids_of(id).into_iter().find(|record| {
            slab.record(*record)
                .is_some_and(|record| record.channel == Channel::BufferLocal(buffer))
        }) else {
            return false;
        };
        let Some(record) = slab.record(outermost).copied() else {
            return false;
        };
        match record.undo {
            UndoTarget::Previous(_) | UndoTarget::Captured(_) => {
                if let Some(slab) = self.owner.slab_mut() {
                    let undo = match record.undo {
                        UndoTarget::Captured(_) => UndoTarget::Captured(SlotValue::new(value)),
                        _ => UndoTarget::Previous(SlotValue::new(value)),
                    };
                    slab.replace_undo(outermost, undo);
                }
            }
            UndoTarget::Inherited { .. } | UndoTarget::Retired => {
                self.owner_canonical_store(id, record.channel, value);
            }
        }
        true
    }

    /// Whether this owner binds ID (`Fdefvaralias`'s let-bound refusal).
    #[inline]
    pub(crate) fn owner_binds(&self, id: SymId) -> bool {
        self.owner.slab().is_some_and(|slab| slab.binds(id))
    }

    /// This owner's default-channel binding of the projected variable ID, if
    /// any: what GNU's `default_value` reads while the running thread binds
    /// the default (`src/data.c:1962-2004`). A Context with no owner bindings
    /// pays one compare of its owner word and a branch.
    #[inline]
    pub(crate) fn owner_default_binding(&self, id: SymId) -> Option<SlotValue> {
        if self.owner.is_dormant() {
            return None;
        }
        self.owner_default_binding_slow(id)
    }

    #[cold]
    #[inline(never)]
    fn owner_default_binding_slow(&self, id: SymId) -> Option<SlotValue> {
        let slab = self.owner.slab()?;
        let projection = self.obarray.thread_projection_of(id)?;
        slab.slot(projection.index(), Channel::Default).value()
    }

    /// Store VALUE in this owner's default binding of the projected variable
    /// ID (`set-default` while the owner binds the default).
    pub(crate) fn store_owner_default(&mut self, id: SymId, value: Value) {
        let Some(projection) = self.obarray.thread_projection_of(id) else {
            return;
        };
        if let Some(slab) = self.owner.slab_mut() {
            slab.set_slot(
                projection.index(),
                Channel::Default,
                SlotState::Bound(SlotValue::new(value)),
            );
        }
    }

    /// `kill-local-variable` of the projected variable ID in the current
    /// buffer, before the local goes: a binding of this owner hiding that
    /// local now hides nothing. Its outermost binding keeps the value the
    /// local held, which GNU's entry carries and restores if the local is
    /// made again (`src/eval.c:3871-3883`).
    pub(crate) fn owner_local_killed(&mut self, id: SymId) {
        let Some(buffer) = self.buffers.current_buffer_id() else {
            return;
        };
        let channel = Channel::BufferLocal(buffer);
        if self.owner.slab().is_none() {
            return;
        }
        let canonical = self.owner_canonical_value(id, channel);
        self.capture_owner_local(id, buffer, canonical);
    }

    /// The local of ID in BUFFER, which held CANONICAL, is gone: this owner's
    /// outermost binding of it keeps CANONICAL to restore if the local is
    /// made again, and the owner's slot for it goes.
    fn capture_owner_local(&mut self, id: SymId, buffer: BufferId, canonical: Value) {
        let channel = Channel::BufferLocal(buffer);
        let Some(slab) = self.owner.slab() else {
            return;
        };
        let ids = slab.record_ids_of(id);
        let Some(outermost) = ids.iter().copied().find(|record| {
            slab.record(*record)
                .is_some_and(|record| record.channel == channel)
        }) else {
            return;
        };
        let Some(record) = slab.record(outermost).copied() else {
            return;
        };
        if let UndoTarget::Inherited { .. } = record.undo
            && let Some(slab) = self.owner.slab_mut()
        {
            slab.replace_undo(outermost, UndoTarget::Captured(SlotValue::new(canonical)));
        }
        self.clear_owner_slot(record.index, channel);
    }

    /// Before `kill-all-local-variables` resets BUFFER's locals: the
    /// variables this owner binds there and the canonical local each binding
    /// hides, for [`Self::owner_locals_reset`].
    #[cold]
    #[inline(never)]
    pub(crate) fn owner_locals_before_reset(&mut self, buffer: BufferId) -> Vec<(SymId, Value)> {
        let channel = Channel::BufferLocal(buffer);
        let Some(slab) = self.owner.slab() else {
            return Vec::new();
        };
        let mut bound: Vec<SymId> = slab
            .live_records()
            .filter(|(_, record)| record.channel == channel)
            .map(|(_, record)| record.sym)
            .collect();
        bound.sort_unstable_by_key(|id| id.0);
        bound.dedup();
        bound
            .into_iter()
            .map(|id| (id, self.owner_canonical_value(id, channel)))
            .collect()
    }

    /// `kill-all-local-variables` reset BUFFER's locals: each binding of
    /// this owner whose local is gone captures what the local held, as
    /// `kill-local-variable` makes it do (GNU's entry keeps its saved value,
    /// `src/eval.c:3871-3883`). BEFORE comes from
    /// [`Self::owner_locals_before_reset`]; no Lisp ran in between, so its
    /// values are still reachable from nothing a collection could miss.
    #[cold]
    #[inline(never)]
    pub(crate) fn owner_locals_reset(&mut self, buffer: BufferId, before: Vec<(SymId, Value)>) {
        let channel = Channel::BufferLocal(buffer);
        for (id, canonical) in before {
            let Some(projection) = self.obarray.thread_projection_of(id) else {
                continue;
            };
            if !self.owner_channel_live(id, projection, channel) {
                self.capture_owner_local(id, buffer, canonical);
            }
        }
    }

    /// BUFFERS were killed: this owner's bindings of their locals restore
    /// nothing at their pops (GNU's `Flocal_variable_p` test fails on a dead
    /// buffer, `src/eval.c:3871`), and their slots go.
    #[cold]
    #[inline(never)]
    pub(crate) fn owner_buffers_killed(&mut self, buffers: &[BufferId]) {
        let Some(slab) = self.owner.slab() else {
            return;
        };
        let dead: Vec<_> = slab
            .live_records()
            .filter(|(_, record)| {
                matches!(record.channel, Channel::BufferLocal(buffer) if buffers.contains(&buffer))
            })
            .map(|(id, record)| (id, record.index, record.channel))
            .collect();
        for (id, index, channel) in dead {
            if let Some(slab) = self.owner.slab_mut() {
                slab.replace_undo(id, UndoTarget::Retired);
            }
            self.clear_owner_slot(index, channel);
        }
    }

    /// This owner's binding of the projected variable ID in BUFFER's local,
    /// if it binds one: what `buffer-local-value` answers for BUFFER, as GNU's
    /// local cell holds the running thread's binding.
    #[cold]
    #[inline(never)]
    pub(crate) fn owner_local_binding(&self, id: SymId, buffer: BufferId) -> Option<SlotValue> {
        let projection = self.obarray.thread_projection_of(id)?;
        self.owner
            .slab()?
            .slot(projection.index(), Channel::BufferLocal(buffer))
            .value()
    }

    /// Whether the current buffer has a local binding of the projected
    /// variable ID.
    pub(crate) fn projected_local_in_current_buffer(&self, id: SymId) -> bool {
        let (Some(projection), Some(buffer)) = (
            self.obarray.thread_projection_of(id),
            self.buffers.current_buffer_id(),
        ) else {
            return false;
        };
        self.owner_channel_live(id, projection, Channel::BufferLocal(buffer))
    }

    /// `make-local-variable` of the projected variable ID made a local in
    /// the current buffer. GNU starts it at the default the running thread
    /// sees (`src/data.c:2245-2348`), which is this owner's default binding
    /// when it has one. A binding of this owner whose local was killed takes
    /// the new local back: the local holds what its outermost binding hid,
    /// and the owner's slot the value the local starts with.
    pub(crate) fn owner_local_made(&mut self, id: SymId) {
        let Some(buffer) = self.buffers.current_buffer_id() else {
            return;
        };
        let channel = Channel::BufferLocal(buffer);
        let Some(projection) = self.obarray.thread_projection_of(id) else {
            return;
        };
        if !self.owner_channel_live(id, projection, channel) {
            return;
        }
        let Some(slab) = self.owner.slab() else {
            return;
        };
        let default = slab.slot(projection.index(), Channel::Default).value();
        let outermost = slab.record_ids_of(id).into_iter().find(|record| {
            slab.record(*record)
                .is_some_and(|record| record.channel == channel)
        });
        let start = match default {
            Some(default) => default.get(),
            None => self.owner_canonical_value(id, Channel::Default),
        };
        match outermost {
            Some(outermost) => {
                let hidden = match self.owner.slab().and_then(|slab| slab.record(outermost)) {
                    Some(OwnerBindingRecord {
                        undo: UndoTarget::Captured(saved),
                        ..
                    }) => Some(*saved),
                    _ => None,
                };
                if let Some(hidden) = hidden {
                    self.owner_canonical_store(id, channel, hidden.get());
                    if let Some(slab) = self.owner.slab_mut() {
                        slab.replace_undo(outermost, UndoTarget::Inherited { suspended: None });
                    }
                }
                if let Some(slab) = self.owner.slab_mut() {
                    slab.set_slot(
                        projection.index(),
                        channel,
                        SlotState::Bound(SlotValue::new(start)),
                    );
                }
            }
            None if default.is_some() => self.owner_canonical_store(id, channel, start),
            None => {}
        }
    }
}

/// The canonical value of the variable PROJECTION wraps without a buffer,
/// as `Obarray::canonical_value` reads an unprojected cell: the plain word,
/// a localized variable's default, a builtin's own storage (none for a
/// per-buffer slot). `None` when void.
pub(super) fn canonical_bufferless_value(projection: &ThreadProjectionDescriptor) -> Option<Value> {
    let value = match projection.arm() {
        CanonicalArm::Plain => projection.plain_value(),
        // SAFETY: a projection's localized arm names a live record of the
        // obarray's BLV pool; its default cell is only read.
        CanonicalArm::Localized(blv) => unsafe { (*blv.blv().as_ptr()).defcell.cons_cdr() },
        CanonicalArm::Forwarded(builtin) => return builtin.descriptor().load(),
    };
    (!value.is_unbound()).then_some(value)
}

/// The owner-aware halves of `eval::variable_access`: cold, reached only by a
/// Context with owner bindings.
impl Context {
    /// The projection behind ID, through its aliases.
    fn projection_behind(&self, id: SymId) -> Option<(SymId, &'static ThreadProjectionDescriptor)> {
        let base = self.obarray.indirect_variable_id(id)?;
        Some((base, self.obarray.thread_projection_of(base)?))
    }

    /// [`Context::store_variable`] of a projected cell.
    #[cold]
    #[inline(never)]
    pub(super) fn store_owner_variable(&mut self, id: SymId, value: Value) {
        debug_assert!(
            self.owner.slab().is_some(),
            "a Context that sees a projected cell has an owner slab"
        );
        let Some((base, projection)) = self.projection_behind(id) else {
            self.obarray
                .set_canonical_value(CanonicalAccess::ROUTED, id, value);
            return;
        };
        let index = projection.index();
        if let Some(slab) = self.owner.slab_mut()
            && slab.slot(index, Channel::Default).value().is_some()
        {
            slab.set_slot(
                index,
                Channel::Default,
                SlotState::Bound(SlotValue::new(value)),
            );
            return;
        }
        self.with_revealed_projection(base, |ctx| {
            ctx.obarray
                .set_canonical_value(CanonicalAccess::STORAGE, base, value);
        });
    }

    /// [`Context::makunbound_variable`] of a projected cell.
    #[cold]
    #[inline(never)]
    pub(super) fn makunbound_owner_variable(&mut self, id: SymId) {
        debug_assert!(
            self.owner.slab().is_some(),
            "a Context that sees a projected cell has an owner slab"
        );
        let Some((base, projection)) = self.projection_behind(id) else {
            self.obarray
                .makunbound_canonical(CanonicalAccess::ROUTED, id);
            return;
        };
        let index = projection.index();
        if let Some(slab) = self.owner.slab_mut()
            && slab.slot(index, Channel::Default).value().is_some()
        {
            slab.set_slot(
                index,
                Channel::Default,
                SlotState::Bound(SlotValue::new(Value::UNBOUND)),
            );
            return;
        }
        self.with_revealed_projection(base, |ctx| {
            ctx.obarray
                .makunbound_canonical(CanonicalAccess::STORAGE, base);
        });
    }
}
