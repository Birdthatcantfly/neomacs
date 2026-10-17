//! Frame-owned face realization, separate from authoritative Lisp definitions.
//!
//! | Knob | Default | Meaning |
//! | --- | --- | --- |
//! | `NEOVM_FACE_REALIZATION_LIFECYCLE` | off | GNU basic/demand realization through Lisp TTY color callbacks. |

use super::{Face, FaceTable};
use crate::emacs_core::value::Value;
use crate::gc_trace::GcTrace;
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

/// Process-wide immutable campaign selector. No mutable Lisp state is shared.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("NEOVM_FACE_REALIZATION_LIFECYCLE").is_ok_and(|value| value == "1")
    })
}

/// Each frame's exclusive mutator owns this cache. Immutable FaceTable handles
/// remain copy-on-write; no cache entry or Lisp value is shared across mutators.
#[derive(Default)]
pub(crate) struct FrameFaceRealizationCache {
    generation: Option<u64>,
    table: Option<FaceTable>,
    entries: FxHashMap<u64, Vec<(Face, Face)>>,
}

fn route_hash(face: &Face) -> u64 {
    let mut hash = FxHasher::default();
    face.color_specifications.foreground.hash(&mut hash);
    face.color_specifications.background.hash(&mut hash);
    face.color_specifications.underline.hash(&mut hash);
    face.color_specifications
        .underline_operations
        .hash(&mut hash);
    face.weight
        .map(|weight| weight.css_weight())
        .hash(&mut hash);
    hash.finish()
}

// Face derives equality through Value's structural `equal` comparison, as
// required by GNU face_attr_equal_p (xfaces.c:4481-4507).

impl std::fmt::Debug for FrameFaceRealizationCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrameFaceRealizationCache")
            .field("generation", &self.generation)
            .field("faces", &self.table.as_ref().map(FaceTable::len))
            .field("attribute_buckets", &self.entries.len())
            .finish()
    }
}

static_assertions::assert_not_impl_any!(FrameFaceRealizationCache: Send, Sync);

impl FrameFaceRealizationCache {
    pub(crate) fn generation_matches(&self, generation: u64) -> bool {
        self.generation == Some(generation)
    }

    pub(crate) fn table(&self, generation: u64) -> Option<&FaceTable> {
        (self.generation == Some(generation))
            .then_some(self.table.as_ref())
            .flatten()
    }

    pub(crate) fn prepare(&mut self, generation: u64, table: FaceTable) {
        self.generation = Some(generation);
        self.table = Some(table);
        self.entries.clear();
    }

    pub(crate) fn invalidate(&mut self) {
        self.generation = None;
    }

    pub(crate) fn lookup(&self, attributes: &Face) -> Option<&Face> {
        self.entries
            .get(&route_hash(attributes))?
            .iter()
            .find_map(|(source, realized)| (source == attributes).then_some(realized))
    }

    pub(crate) fn insert(&mut self, attributes: Face, realized: Face) {
        let entries = self.entries.entry(route_hash(&attributes)).or_default();
        if let Some((_, previous)) = entries.iter_mut().find(|(source, _)| *source == attributes) {
            *previous = realized;
        } else {
            entries.push((attributes, realized));
        }
    }
}

impl GcTrace for FrameFaceRealizationCache {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        if let Some(table) = &self.table {
            table.trace_roots(roots);
        }
        for entries in self.entries.values() {
            for (source, realized) in entries {
                source.trace_roots(roots);
                realized.trace_roots(roots);
            }
        }
    }
}

#[cfg(test)]
#[path = "realization/tests/cache_test.rs"]
mod tests;

/// GNU's basic face names in realization order, derived from its typed roles.
pub fn basic_face_names() -> impl Iterator<Item = &'static str> {
    crate::emacs_core::xfaces::realization::basic_face_names()
}
