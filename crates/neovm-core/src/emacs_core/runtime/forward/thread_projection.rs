//! The internal thread-projection descriptor (Phase 0 step 2).
//!
//! When a second Lisp thread is admitted, every mutable variable cell is to
//! become `SYMBOL_FORWARDED` to one of these descriptors, and dynamic
//! bindings then live in per-owner slots selected by the descriptor's
//! [`TlsIndex`]. What the symbol held before admission stays behind the
//! descriptor as its canonical storage: the LOGICAL redirect
//! ([`CanonicalArm`]) and, for a plain variable, the canonical value word.
//!
//! # Encoding: a per-buffer header no buffer has
//!
//! A descriptor begins with a real [`LispBufferObjFwd`] header (`repr(C)`,
//! first field) whose `offset` is [`THREAD_PROJECTION_OFFSET`] -- a slot
//! index no buffer has, pinned below against `BUFFER_SLOT_COUNT` -- with
//! `local_flags_idx = -1` and `default = UNBOUND`. The family byte stays
//! `BufferObj`, so `LispFwdType`, `ForwardSlot` and every dispatch over them
//! are exactly what they were: no switch sees a new value, and none is
//! lowered differently. A header is neither `Copy` nor `Clone` and real
//! per-buffer forwarders take a typed `BufferSlot`, so the marker only ever
//! heads a descriptor ([`LispBufferObjFwd::thread_projection`]).
//!
//! Every hot reader refuses such a cell with a check it already makes for
//! per-buffer slots, before touching any storage:
//! - `LispFwd::load` answers `None` (the per-buffer arm), so the VM's
//!   forwarded varref and the interpreter fall back to their general walks;
//! - `LispBufferObjFwd::value_in` fails its `off < slots.len()` test and
//!   answers `default`, which is UNBOUND, so the cached read tier refuses on
//!   its `is_unbound` test;
//! - the header's `offset` is private to the forward module; a slot is
//!   reachable only through `LispBufferObjFwd::buffer_slot`
//!   (`BufferSlot::from_u16`) or `slot_index().below(len)`, the checks
//!   every slot access already made, and both reject the marker;
//! - JIT classification (`FwdKind::of`) gives per-buffer slots no inline
//!   shape, so compiled sites refuse it on their redirect guard;
//! - the projected cell also carries the host-projection bit, which every
//!   checked writer refuses.
//!
//! The slow paths those refusals reach route a projected cell to its
//! canonical storage (`eval::thread_projection`), each from a branch inside
//! its existing per-buffer handling.
//!
//! # Reachability
//!
//! Admission does not exist yet. The constructor, the World's table and the
//! cell transitions INTO the projected state from an unprojected variable
//! are compiled for tests only, so production code reaches the routing only
//! through a projected cell, which it cannot create.

use super::{LispBufferObjFwd, LispFwd, LispObjFwd};
use crate::buffer::buffer::BUFFER_SLOT_COUNT;
use crate::emacs_core::symbol::BlvPtr;
use crate::emacs_core::value::Value;
use std::num::NonZeroU32;

/// The per-buffer slot index a projection's header carries: no buffer has
/// it, so every slot access derived from the header fails its existing
/// bounds or validity check.
pub const THREAD_PROJECTION_OFFSET: u16 = u16::MAX;

const _: () = assert!(BUFFER_SLOT_COUNT < THREAD_PROJECTION_OFFSET as usize);

/// A projected variable's dense slot number in every owner's binding vector.
///
/// One per canonical base variable (aliases resolve first), issued by the
/// World's [`ThreadProjectionTable`]. Nonzero, so `Option<TlsIndex>` is a
/// `u32`. It names a slot; it grants no access to another heap or owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TlsIndex(NonZeroU32);

static_assertions::assert_eq_size!(TlsIndex, Option<TlsIndex>, u32);

impl TlsIndex {
    /// The slot's position in a zero-based owner vector.
    #[inline]
    pub fn slot(self) -> usize {
        // `NonZeroU32` fits `usize` on every supported target.
        (self.0.get() - 1) as usize
    }
}

/// The buffer-local record a projected `SYMBOL_LOCALIZED` variable keeps.
/// The obarray's BLV pool still owns, traces and frees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalBlv(BlvPtr);

static_assertions::assert_not_impl_any!(CanonicalBlv: Send, Sync);

impl CanonicalBlv {
    /// The record a localized cell names.
    #[inline]
    pub(crate) fn new(blv: BlvPtr) -> Self {
        Self(blv)
    }

    /// The record, for the obarray that owns it.
    #[inline]
    pub(crate) fn blv(self) -> BlvPtr {
        self.0
    }
}

/// A GNU built-in forwarder a projected variable keeps as its canonical
/// storage: never a projection's own header, so a projection never wraps a
/// projection.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinForwarder(&'static LispFwd);

static_assertions::assert_not_impl_any!(BuiltinForwarder: Send, Sync);

impl BuiltinForwarder {
    /// FWD as a canonical backing, or `None` for a projection's header.
    #[inline]
    pub fn new(fwd: &'static LispFwd) -> Option<Self> {
        fwd.thread_projection().is_none().then_some(Self(fwd))
    }

    /// The GNU descriptor.
    #[inline]
    pub fn descriptor(self) -> &'static LispFwd {
        self.0
    }
}

impl PartialEq for BuiltinForwarder {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for BuiltinForwarder {}

/// A projected variable's LOGICAL redirect: what its cell would hold if it
/// were not projected. `Varalias` has no arm: an alias stays an alias and
/// resolves to its projected base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalArm {
    /// GNU `SYMBOL_PLAINVAL`: the value is the descriptor's plain word.
    Plain,
    /// GNU `SYMBOL_LOCALIZED`: the variable's buffer-local record.
    Localized(CanonicalBlv),
    /// GNU `SYMBOL_FORWARDED`: the built-in descriptor, still rooted where
    /// it was.
    Forwarded(BuiltinForwarder),
}

static_assertions::assert_not_impl_any!(CanonicalArm: Send, Sync);

/// The thread-projection descriptor: a `SYMBOL_FORWARDED` cell's target once
/// its variable is owner-local. See the module documentation for why its
/// first field is a per-buffer header.
///
/// Fields are private and the only constructor is the World table's, so a
/// header carrying [`THREAD_PROJECTION_OFFSET`] heads one of these.
#[repr(C)]
pub struct ThreadProjectionDescriptor {
    header: LispBufferObjFwd,
    index: TlsIndex,
    /// The canonical value word while the logical arm is `Plain`: a
    /// `DEFVAR_LISP`-shaped slot registered with the obarray's forwarder
    /// roots, so the existing root scan traces it and every store notes the
    /// overwritten word first ([`LispObjFwd::set`]). It holds UNBOUND for a
    /// void variable.
    plain: &'static LispObjFwd,
    /// The logical redirect. A projected variable can change arm the way an
    /// unprojected one does (`make-local-variable` localizes a plain or
    /// forwarded variable) without leaving its projection.
    arm: std::cell::Cell<CanonicalArm>,
    /// Whether the cell carried the host-projection bit before projection,
    /// so the logical cell gets it back while it is revealed.
    host_projected: bool,
}

static_assertions::assert_not_impl_any!(ThreadProjectionDescriptor: Copy, Clone, Send, Sync);
const _: () = {
    use std::mem::{needs_drop, offset_of};
    // Leaked like every forwarder body: drop glue would be stranded.
    assert!(!needs_drop::<ThreadProjectionDescriptor>());
    // The header is the body's start, so the cell's `&LispFwd` and a
    // per-buffer reader's `&LispBufferObjFwd` both address it.
    assert!(offset_of!(ThreadProjectionDescriptor, header) == 0);
};

impl ThreadProjectionDescriptor {
    /// The variable's slot in every owner's binding vector.
    #[inline]
    pub fn index(&self) -> TlsIndex {
        self.index
    }

    /// The variable's logical redirect.
    #[inline]
    pub fn arm(&self) -> CanonicalArm {
        self.arm.get()
    }

    /// Record a change of the variable's logical redirect.
    #[inline]
    pub(crate) fn set_arm(&self, arm: CanonicalArm) {
        self.arm.set(arm);
    }

    /// The canonical plain word ([`Value::UNBOUND`] when void). Meaningful
    /// while the arm is [`CanonicalArm::Plain`].
    #[inline]
    pub fn plain_value(&self) -> Value {
        self.plain.get()
    }

    /// Store the canonical plain word; the overwritten word is noted for a
    /// running mark first.
    #[inline]
    pub(crate) fn set_plain_value(&self, value: Value) {
        self.plain.set(value);
    }

    /// The forwarder whose slot is the plain word, for root registration
    /// by the obarray that projects the variable.
    #[cfg(test)]
    #[inline]
    pub(crate) fn plain_root(&self) -> &'static LispFwd {
        use super::FwdDescriptor;
        self.plain.header()
    }

    /// Whether the logical cell carries the host-projection bit.
    #[inline]
    pub fn host_projected(&self) -> bool {
        self.host_projected
    }

    /// The forwarder governing stores to the variable: none for a plain
    /// cell, the BLV's (if any) for a localized one, the builtin itself for a
    /// forwarded one.
    #[inline]
    pub(crate) fn logical_forwarder(&self) -> Option<&'static LispFwd> {
        match self.arm() {
            CanonicalArm::Plain => None,
            // SAFETY: a projection's localized arm names a live record of
            // the obarray's BLV pool; only its forwarder field is read.
            CanonicalArm::Localized(blv) => unsafe { (*blv.blv().as_ptr()).fwd },
            CanonicalArm::Forwarded(builtin) => Some(builtin.descriptor()),
        }
    }

    /// The per-buffer header a symbol cell stores.
    #[inline]
    pub fn header(&self) -> &LispFwd {
        use super::FwdDescriptor;
        self.header.header()
    }
}

impl LispBufferObjFwd {
    /// The thread projection this per-buffer header heads, if it carries the
    /// marker. Called only where a real per-buffer forwarder cannot be:
    /// after a slot access that the marker made fail.
    #[inline]
    pub fn thread_projection(&self) -> Option<&ThreadProjectionDescriptor> {
        if self.offset != THREAD_PROJECTION_OFFSET {
            return None;
        }
        // SAFETY: a `LispBufferObjFwd` carrying THREAD_PROJECTION_OFFSET is
        // only ever built by `ThreadProjectionTable`, as the first `repr(C)`
        // field (offset 0, pinned above) of a leaked, never-freed
        // `ThreadProjectionDescriptor`: `alloc_buffer_objfwd` takes a typed
        // `BufferSlot` (below BUFFER_SLOT_COUNT, pinned below the marker),
        // the struct's `ty` and `offset` are private to this module, and it
        // is neither `Copy` nor `Clone`, so no header with the marker exists
        // apart from its descriptor. The cast therefore names that
        // descriptor, which outlives `self`'s borrow.
        Some(unsafe { &*std::ptr::from_ref(self).cast::<ThreadProjectionDescriptor>() })
    }
}

impl LispFwd {
    /// The thread projection this header heads, if it is one.
    #[inline]
    pub fn thread_projection(&self) -> Option<&ThreadProjectionDescriptor> {
        self.as_buffer_obj_fwd()?.thread_projection()
    }
}

impl std::fmt::Debug for ThreadProjectionDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadProjectionDescriptor")
            .field("index", &self.index)
            .field("arm", &self.arm.get())
            .field("plain", &self.plain)
            .field("host_projected", &self.host_projected)
            .finish_non_exhaustive()
    }
}

/// The World's registry of thread projections: slot allocation, and the one
/// authority that maps a cell's header back to its descriptor.
///
/// Descriptors are leaked like every forwarder body, since symbol cells hold
/// `&'static LispFwd`; the table owns the variable-to-slot assignment.
///
/// Test-only, like the World field that owns it, until stopped admission
/// exists.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct ThreadProjectionTable {
    issued: Vec<&'static ThreadProjectionDescriptor>,
    // The table lives in a confined World.
    _thread_confined: std::marker::PhantomData<*const ()>,
}

#[cfg(test)]
static_assertions::assert_not_impl_any!(ThreadProjectionTable: Send, Sync, Clone);

#[cfg(test)]
impl ThreadProjectionTable {
    /// How many projections the table has issued.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.issued.len()
    }

    /// Whether no projection has been issued.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.issued.is_empty()
    }

    /// The projection FWD heads, if this table issued it.
    ///
    /// Safe by construction: the marker is checked first (one compare a
    /// real per-buffer header always fails), then the address is matched
    /// against the descriptors this table issued, so a header copied out of
    /// a projection is never mistaken for one.
    pub(crate) fn projection(&self, fwd: &LispFwd) -> Option<&'static ThreadProjectionDescriptor> {
        if fwd.as_buffer_obj_fwd()?.offset != THREAD_PROJECTION_OFFSET {
            return None;
        }
        self.issued
            .iter()
            .copied()
            .find(|issued| std::ptr::eq(issued.header(), fwd))
    }
}

#[cfg(test)]
impl ThreadProjectionTable {
    /// Issue the next slot over logical redirect ARM, with PLAIN as the
    /// canonical plain word (UNBOUND unless ARM is `Plain`). Test-only until
    /// stopped admission exists: no production path builds a projection.
    /// The caller registers [`ThreadProjectionDescriptor::plain_root`] with
    /// its obarray's forwarder roots.
    pub(crate) fn project_for_test(
        &mut self,
        arm: CanonicalArm,
        plain: Value,
        host_projected: bool,
    ) -> &'static ThreadProjectionDescriptor {
        let next = u32::try_from(self.issued.len() + 1)
            .ok()
            .and_then(NonZeroU32::new)
            .expect("thread-projection index space exhausted");
        let descriptor: &'static ThreadProjectionDescriptor =
            Box::leak(Box::new(ThreadProjectionDescriptor {
                header: LispBufferObjFwd {
                    ty: super::LispFwdType::BufferObj,
                    offset: THREAD_PROJECTION_OFFSET,
                    local_flags_idx: -1,
                    predicate: crate::buffer::buffer::BufferSlotPredicate::Unrestricted,
                    default: Value::UNBOUND,
                },
                index: TlsIndex(next),
                plain: super::alloc_objfwd(plain),
                arm: std::cell::Cell::new(arm),
                host_projected,
            }));
        self.issued.push(descriptor);
        descriptor
    }

    /// The descriptor issued for INDEX, if any.
    pub(crate) fn get(&self, index: TlsIndex) -> Option<&'static ThreadProjectionDescriptor> {
        self.issued.get(index.slot()).copied()
    }
}
