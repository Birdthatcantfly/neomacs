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

use crate::buffer::BufferId;
use crate::emacs_core::forward::TlsIndex;
use crate::emacs_core::intern::SymId;
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
        self.records
            .iter()
            .enumerate()
            .filter_map(|(position, record)| {
                let record = record.as_ref().filter(|record| record.sym == sym)?;
                let depth = u32::try_from(position + 1).ok().and_then(NonZeroU32::new)?;
                Some(OwnerRecordId {
                    depth,
                    generation: record.generation,
                })
            })
            .collect()
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
