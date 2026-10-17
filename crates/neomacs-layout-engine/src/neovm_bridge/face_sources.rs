//! Independently rooted lface source attributes retained by layout snapshots.

use neovm_core::emacs_core::Value;
use neovm_core::face::Face;
#[cfg(test)]
use neovm_core::face::FaceTable;
use neovm_core::gc_trace::GcTrace;
use neovm_core::tagged::transport::{SharedRoot, SharedRootError};
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
        })
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
