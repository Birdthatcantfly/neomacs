//! Independently rooted lface source attributes retained by layout snapshots.

use super::ResolvedFace;
use super::face_colors::{TtyPaint, TtyPaintKey, TtyPaintMemo};
use neovm_core::emacs_core::Value;
use neovm_core::face::Face;
#[cfg(test)]
use neovm_core::face::FaceTable;
use neovm_core::gc_trace::GcTrace;
use neovm_core::tagged::transport::{SharedRoot, SharedRootError};
use neovm_core::window::FrameId;
use std::cell::Cell;
use std::sync::Arc;

/// A logical face whose Lisp-valued attributes remain alive independently of
/// the frame cache generation or authoritative face-definition tables.
///
/// Construction and attribute access belong to the current heap's mutator.
/// The immutable Arc payload shares ownership within that mutator only: Face
/// contains local Values, so this type deliberately remains !Send and !Sync.
/// Root leases protect family/foundry/stipple references across callbacks,
/// collection, cache invalidation, and retained window snapshots. Merges that
/// retain their parent's Lisp references share its existing root bundle.
#[derive(Debug)]
pub(super) struct RootedFace {
    attributes: Face,
    roots: Arc<FaceRoots>,
    /// The last terminal paint realized from this source, keyed by frame and
    /// binding generation. Realization revisits the same interned sources far
    /// more often than it meets new ones, so parking one slot beside the
    /// content it belongs to serves every revisit without a probe structure:
    /// the entry retires with its stamp (GNU `face_change`, src/xfaces.c:349)
    /// or dies with this allocation. Interior mutability keeps the Arc
    /// API; this type is already !Sync by construction.
    tty_paint: Cell<Option<TtyPaintMemo>>,
}

#[derive(Debug)]
struct FaceRoots {
    values: Vec<Value>,
    _leases: Vec<SharedRoot>,
}

static_assertions::assert_not_impl_any!(RootedFace: Send, Sync, Copy);

/// Roots every definition the pure resolver may inspect after arbitrary Lisp
/// callbacks, including faces not yet chosen for a glyph. The owning resolver
/// retains this one lease bundle until its immutable table snapshot is dropped.
/// Production binding shares the frame cache's lease, so warm redisplay need
/// not trace/root every definition again. Existing clones survive invalidation.
/// Handles may retire on any thread; definition access remains mutator-local.
#[derive(Debug)]
pub(super) struct FaceTableRoots {
    _leases: Arc<[SharedRoot]>,
}

static_assertions::assert_impl_all!(FaceTableRoots: Send, Sync);

impl From<Arc<[SharedRoot]>> for FaceTableRoots {
    fn from(leases: Arc<[SharedRoot]>) -> Self {
        Self { _leases: leases }
    }
}

#[cfg(test)]
impl FaceTableRoots {
    pub(super) fn new(table: &FaceTable) -> Result<Self, SharedRootError> {
        let mut values = Vec::new();
        table.trace_roots(&mut values);
        // SAFETY: binding runs on the installed Context's mutator, while its
        // authoritative/frame face table still roots every definition in this
        // freshly cloned snapshot. No callback or collection intervenes before
        // this batch admission, whose private vector allocation cannot collect.
        // The lease survives later table replacement, cache invalidation and
        // collection until the resolver drops its immutable source snapshot.
        let leases = unsafe { SharedRoot::batch_from_current_heap(&values) }?;
        Ok(Self::from(Arc::from(leases)))
    }
}

impl RootedFace {
    pub(super) fn attributes(&self) -> &Face {
        &self.attributes
    }

    /// A pure resolver without an installed heap has no Lisp realization
    /// capability. Its caller keeps the ordinary unbound paint-only result.
    pub(super) fn new(attributes: Face) -> Result<Self, SharedRootError> {
        Self::with_base(attributes, None)
    }

    pub(super) fn with_base(
        attributes: Face,
        base: Option<&Self>,
    ) -> Result<Self, SharedRootError> {
        let mut values = Vec::new();
        attributes.trace_roots(&mut values);
        if let Some(base) = base
            && values == base.roots.values
        {
            return Ok(Self {
                attributes,
                roots: Arc::clone(&base.roots),
                tty_paint: Cell::new(None),
            });
        }
        // SAFETY: callers construct attributes only from the installed
        // mutator's rooted face table, buffer/string properties, or an already
        // rooted source face. No Lisp callback or collection occurs between
        // cloning/merging those sources and this admission. The installed
        // heap traces the shared-root table; batch admission allocates its
        // private immutable vector without collecting or evaluating Lisp.
        let leases = unsafe { SharedRoot::batch_from_current_heap(&values) }?;
        Ok(Self {
            attributes,
            roots: Arc::new(FaceRoots {
                values,
                _leases: leases,
            }),
            tty_paint: Cell::new(None),
        })
    }

    /// The paint this source last realized for `frame` at binding generation
    /// `face_change_count`, when `face` is still in the recorded probe state.
    ///
    /// A hit is GNU serving a repeated merged-lface lookup from the frame
    /// face cache without re-running `realize_tty_face` (`lookup_face`,
    /// src/xfaces.c:5064-5093). Like `lookup_face`, which keys the frame
    /// cache by the merged lface and never by the realized `struct face`,
    /// the memo matches the PRE-realization state a probe arrives in: the
    /// recorded key must be captured before `install_realized_tty_colors`
    /// writes the realized paint into the face, or every probe of the
    /// unrealized entry state misses by construction.
    pub(super) fn cached_tty_paint(
        &self,
        frame: FrameId,
        face_change_count: u64,
        face: &ResolvedFace,
    ) -> Option<TtyPaint> {
        let memo = self.tty_paint.get()?;
        (memo.frame == frame
            && memo.face_change_count == face_change_count
            && memo.key.matches(face))
        .then(|| memo.paint)
    }

    /// Retain one realized terminal paint beside its source. `probe_key` is
    /// the entry state the next probe of this source will arrive in — the
    /// caller captures `TtyPaintKey::of(face)` BEFORE installing the
    /// realized colors, since the paint stored here is read back only
    /// through [`Self::cached_tty_paint`]'s unrealized-state match.
    pub(super) fn record_tty_paint(
        &self,
        frame: FrameId,
        face_change_count: u64,
        probe_key: TtyPaintKey,
        face: &ResolvedFace,
    ) {
        self.tty_paint.set(Some(TtyPaintMemo {
            frame,
            face_change_count,
            key: probe_key,
            paint: TtyPaint::snapshot(face),
        }));
    }

    /// The paint slot's current contents, journaled before a speculative
    /// routed-row attempt overwrites it (see `FaceCacheRewind::TtyPaint`).
    pub(super) fn tty_paint_slot(&self) -> Option<TtyPaintMemo> {
        self.tty_paint.get()
    }

    /// Restore a journaled paint slot after a refused routed-row attempt.
    pub(super) fn rewind_tty_paint(&self, previous: Option<TtyPaintMemo>) {
        self.tty_paint.set(previous);
    }
}

impl AsRef<Face> for RootedFace {
    fn as_ref(&self) -> &Face {
        &self.attributes
    }
}

impl PartialEq for RootedFace {
    fn eq(&self, other: &Self) -> bool {
        self.attributes == other.attributes
    }
}

#[cfg(test)]
#[path = "tests/face_sources_test.rs"]
mod tests;
