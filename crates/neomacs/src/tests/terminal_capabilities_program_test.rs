use super::*;
use neomacs_display_protocol::TerminalColor;
use neomacs_display_protocol::tty_capabilities::ColorGround;

#[test]
fn prepared_colors_match_one_shot_for_selected_parameter_shapes() {
    for (foreground, background, separate, depth, color) in [
        (
            "F%p1%d;",
            "B%p1%d;",
            false,
            TtyColorDepth::Indexed(256),
            TerminalColor::Indexed(123),
        ),
        (
            "F%p1%d,%p2%d,%p3%d;",
            "B%p1%d,%p2%d,%p3%d;",
            true,
            TtyColorDepth::Direct(TtyDirectColorRoute::Setrgbf),
            TerminalColor::Direct {
                r: 17,
                g: 34,
                b: 51,
            },
        ),
        (
            "F%p1%d;",
            "B%p1%d;",
            false,
            TtyColorDepth::Direct(TtyDirectColorRoute::RgbFlag),
            TerminalColor::Direct {
                r: 17,
                g: 34,
                b: 51,
            },
        ),
    ] {
        let prepared = color_capabilities(
            b"RESET".to_vec(),
            Some(foreground.as_bytes().to_vec()),
            Some(background.as_bytes().to_vec()),
            if separate {
                ColorParameterShape::Separate
            } else {
                ColorParameterShape::Packed
            },
            depth,
        );
        let reference = TtyColorCapabilities::new(
            b"RESET".to_vec(),
            Some(foreground.as_bytes().to_vec()),
            Some(background.as_bytes().to_vec()),
            separate,
            depth,
            TerminfoExpander::new(|sequence, parameters| {
                let bytes =
                    neomacs_terminfo::expand_numeric(sequence, numeric_parameters(parameters)?)
                        .ok()?;
                (!bytes.is_empty()).then_some(bytes)
            }),
        );
        assert_eq!(prepared, reference);
        for ground in [ColorGround::Foreground, ColorGround::Background] {
            assert_eq!(
                prepared.ground_sequence(ground, color),
                reference.ground_sequence(ground, color)
            );
        }
    }
}

#[test]
fn invalid_prepared_color_does_not_change_capability_presence() {
    let mut database = FakeCapabilityDatabase::bare()
        .with_string("op", "RESET")
        .with_string("AF", "%s")
        .with_string("AB", "B%p1%d")
        .with_number("Co", 256);
    let caps = resolve_tty_attribute_capabilities(&mut database, "");
    assert_eq!(caps.color_cells(), 256);
    let colors = caps.colors.entry().unwrap();
    assert_eq!(colors.orig_pair(), b"RESET");
    assert_eq!(
        colors.ground_sequence(ColorGround::Foreground, TerminalColor::Indexed(7)),
        None
    );
    assert_eq!(
        colors.ground_sequence(ColorGround::Background, TerminalColor::Indexed(7)),
        Some(b"B7".to_vec())
    );
}

#[test]
fn prepared_colors_retain_dynamic_native_division_checks() {
    let colors = color_capabilities(
        b"RESET".to_vec(),
        Some(b"%p1%gA%/%d".to_vec()),
        None,
        ColorParameterShape::Packed,
        TtyColorDepth::Indexed(256),
    );
    neomacs_terminfo::expand_numeric(b"%{2}%PA", [0; 9]).unwrap();
    assert_eq!(
        colors.ground_sequence(ColorGround::Foreground, TerminalColor::Indexed(8)),
        Some(b"4".to_vec())
    );
    neomacs_terminfo::expand_numeric(b"%{0}%PA", [0; 9]).unwrap();
    assert_eq!(
        colors.ground_sequence(ColorGround::Foreground, TerminalColor::Indexed(8)),
        Some(b"0".to_vec())
    );
}

#[test]
fn prepared_numeric_expander_retains_generic_fallback_and_parameter_bounds() {
    let known = b"K%p1%d".as_slice();
    let expander = NumericExpander::new(std::iter::once(known));
    assert_eq!(
        expander.expand(known, TerminfoParameters::One(7)),
        Some(b"K7".to_vec())
    );
    assert_eq!(
        expander.expand(b"U%p1%d", TerminfoParameters::One(8)),
        Some(b"U8".to_vec())
    );
    assert_eq!(
        expander.expand(known, TerminfoParameters::One(u32::MAX)),
        None
    );
    assert_eq!(expander.expand(b"%s", TerminfoParameters::One(7)), None);
}
