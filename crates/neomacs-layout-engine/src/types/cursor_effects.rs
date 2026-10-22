//! Immutable native effect payloads carried by layout input snapshots.
use neomacs_display_protocol::effect_config::EffectsConfig;
use std::sync::Arc;

/// One complete, immutable parsed cursor-effect profile.
///
/// Clones share native owned data only: there are no Lisp or GC references.
/// The private Arc exposes no mutation; reparsing produces a fresh profile.
/// Independent threads may read a shared profile, while a WindowParams still
/// obeys its existing thread confinement through its image-catalog handle.
/// Transport publication makes its own owned EffectsConfig at the old boundary.
#[repr(transparent)]
#[derive(Clone, Debug)]
pub struct SharedCursorEffects(Arc<EffectsConfig>);

impl From<EffectsConfig> for SharedCursorEffects {
    fn from(config: EffectsConfig) -> Self {
        Self(Arc::new(config))
    }
}

impl AsRef<EffectsConfig> for SharedCursorEffects {
    fn as_ref(&self) -> &EffectsConfig {
        self.0.as_ref()
    }
}

impl std::ops::Deref for SharedCursorEffects {
    type Target = EffectsConfig;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

static_assertions::assert_impl_all!(EffectsConfig: Send, Sync);
static_assertions::assert_impl_all!(SharedCursorEffects: Send, Sync, Clone);
const _: () =
    assert!(std::mem::size_of::<Option<SharedCursorEffects>>() == std::mem::size_of::<usize>());

#[cfg(test)]
#[path = "cursor_effects/tests/cursor_effects_test.rs"]
mod tests;
