use super::*;

const FUNCTION: TerminfoExpander = TerminfoExpander::new(|source, _| Some(source.to_vec()));

fn legacy_colors() -> TtyColorCapabilities {
    TtyColorCapabilities::new(
        b"RESET".to_vec(),
        Some(b"bytes".to_vec()),
        None,
        false,
        TtyColorDepth::Indexed(256),
        FUNCTION,
    )
}

#[test]
fn prepared_expander_retains_const_function_and_source_identity_semantics() {
    static_assertions::assert_impl_all!(TerminfoExpander: Clone, Copy, Send, Sync);
    // Existing callers can copy the public wrapper without Clone or ownership
    // changes. Its constructor, consuming expand and Debug remain original.
    let copied = FUNCTION;
    let again = copied;
    assert_eq!(
        copied.expand(b"bytes", TerminfoParameters::One(7)),
        again.expand(b"bytes", TerminfoParameters::One(7))
    );
    let legacy = legacy_colors();
    let prepared = legacy
        .clone()
        .with_prepared_expander(|source, _| Some(source.to_vec()));
    assert_eq!(legacy, prepared);
    assert_eq!(format!("{legacy:?}"), format!("{prepared:?}"));
    assert_eq!(prepared, prepared.clone());
    assert_eq!(format!("{FUNCTION:?}"), "TerminfoExpander(..)");
    assert_eq!(
        legacy.ground_sequence(ColorGround::Foreground, crate::TerminalColor::Indexed(7)),
        prepared.ground_sequence(ColorGround::Foreground, crate::TerminalColor::Indexed(7))
    );
}

#[test]
fn prepared_expander_thread_contract_and_shared_payload() {
    static_assertions::assert_impl_all!(ColorExpansion: Send, Sync);
    static_assertions::assert_impl_all!(TtyColorCapabilities: Send, Sync);
    let colors = legacy_colors().with_prepared_expander(|source, _| Some(source.to_vec()));
    let ColorExpansion::Prepared(owned) = &colors.expand else {
        panic!("prepared variant");
    };
    assert_eq!(std::sync::Arc::strong_count(owned), 1);
    for _ in 0..10 {
        assert_eq!(
            colors.ground_sequence(ColorGround::Foreground, crate::TerminalColor::Indexed(1)),
            Some(b"bytes".to_vec())
        );
    }
    assert_eq!(std::sync::Arc::strong_count(owned), 1);
    let clone = colors.clone();
    let ColorExpansion::Prepared(other) = &clone.expand else {
        panic!("prepared clone");
    };
    assert!(std::sync::Arc::ptr_eq(owned, other));
}
