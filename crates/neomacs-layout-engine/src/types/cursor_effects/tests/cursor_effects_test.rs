//! Child test module of types::cursor_effects; no environment or TLS overrides.
use super::*;
use neomacs_display_protocol::effect_config::FrameRate;
use std::time::Duration;

fn complete_profile() -> EffectsConfig {
    let mut config = EffectsConfig::default();
    config.cursor_glow.enabled = true;
    config.cursor_color_cycle.fps = FrameRate::new(37).expect("nonzero test rate");
    config.indent_guides.rainbow_enabled = true;
    config.indent_guides.rainbow_colors = vec![(0.125, 0.25, 0.5, 0.75), (0.75, 0.5, 0.25, 0.125)];
    config.idle_dim.delay = Duration::from_millis(1234);
    config.idle_dim.fade_duration = Duration::from_millis(567);
    config
}

#[test]
fn cursor_effects_clone_shares_complete_native_profile() {
    let expected = complete_profile();
    let shared = SharedCursorEffects::from(expected.clone());
    let cloned = shared.clone();
    let coerced: &EffectsConfig = &shared;
    assert!(
        std::ptr::eq(coerced, shared.as_ref()),
        "immutable Deref preserves the payload borrow"
    );
    assert!(std::ptr::eq(shared.as_ref(), cloned.as_ref()));
    assert_eq!(shared.as_ref(), &expected);
    assert_eq!(cloned.as_ref(), &expected);
    drop(shared);
    assert_eq!(
        cloned.as_ref(),
        &expected,
        "clone owns its payload lifetime"
    );
}

#[test]
fn cursor_effects_fresh_replacement_and_owned_output_are_isolated() {
    let expected = complete_profile();
    let shared = SharedCursorEffects::from(expected.clone());
    let still_live = shared.clone();
    let mut output_owned = shared.as_ref().clone();
    let mut next_config = expected.clone();
    next_config.indent_guides.rainbow_colors[0].0 = 0.875;
    next_config.cursor_glow.enabled = false;
    let replacement = SharedCursorEffects::from(next_config.clone());
    assert!(!std::ptr::eq(still_live.as_ref(), replacement.as_ref()));
    assert_eq!(replacement.as_ref(), &next_config);
    assert_eq!(still_live.as_ref(), &expected);
    output_owned.indent_guides.rainbow_colors.clear();
    output_owned.idle_dim.delay = Duration::ZERO;
    assert_eq!(still_live.as_ref(), &expected);
    assert_eq!(replacement.as_ref(), &next_config);
    assert_ne!(output_owned, expected);
}

#[test]
fn cursor_effects_option_is_pointer_sized_and_none_stays_absent() {
    static_assertions::assert_impl_all!(SharedCursorEffects: Send, Sync, Clone);
    static_assertions::assert_not_impl_any!(crate::types::WindowParams: Send, Sync);
    assert_eq!(
        std::mem::size_of::<Option<SharedCursorEffects>>(),
        std::mem::size_of::<usize>()
    );
    assert!(
        std::mem::size_of::<crate::types::WindowParams>() < 1024,
        "layout params should not inline the complete optional effects schema"
    );
    let absent: Option<SharedCursorEffects> = None;
    assert!(absent.clone().is_none());
    assert!(
        absent
            .as_ref()
            .map(|effect| effect.as_ref().clone())
            .is_none()
    );
}

#[test]
fn cursor_effects_visual_spec_clone_keeps_profile_and_native_fields() {
    let expected = complete_profile();
    let spec = crate::types::VisualCursorSpec {
        id: -17,
        charpos: 29,
        cursor_kind: neomacs_display_protocol::frame_glyphs::CursorKind::Bar,
        cursor_bar_width: neomacs_display_protocol::cursor::CursorBarWidth::new(3),
        color: 0x12_34_56,
        effects: Some(expected.clone().into()),
    };
    let cloned = spec.clone();
    assert_eq!(cloned.id, spec.id);
    assert_eq!(cloned.charpos, spec.charpos);
    assert_eq!(cloned.cursor_kind, spec.cursor_kind);
    assert_eq!(cloned.cursor_bar_width, spec.cursor_bar_width);
    assert_eq!(cloned.color, spec.color);
    let original = spec.effects.as_ref().expect("profile");
    let copied = cloned.effects.as_ref().expect("profile");
    assert!(std::ptr::eq(original.as_ref(), copied.as_ref()));
    assert_eq!(copied.as_ref(), &expected);
}
