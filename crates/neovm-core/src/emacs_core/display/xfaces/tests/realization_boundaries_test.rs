use super::*;

#[test]
fn default_realization_binding_restores_after_lisp_error() {
    let mut context = super::super::super::eval::Context::new();
    context.set_variable("inhibit-redisplay", Value::NIL);
    let mut scope = RealizationBinding::enter(&mut context, "inhibit-redisplay", Value::T)
        .expect("ordinary special variable accepts its binding");
    assert_eq!(
        scope.context().variable_value_by_name("inhibit-redisplay"),
        Some(Value::T)
    );
    let failure: Result<(), Flow> = Err(crate::emacs_core::error::signal(
        "error",
        vec![Value::string("realization advice failure")],
    ));
    assert!(scope.finish(failure).is_err());
    assert_eq!(
        context.variable_value_by_name("inhibit-redisplay"),
        Some(Value::NIL)
    );
}

#[test]
fn default_realization_binding_drop_restores_native_state() {
    let mut context = super::super::super::eval::Context::new();
    context.set_variable("inhibit-redisplay", Value::fixnum(7));
    {
        let _scope = RealizationBinding::enter(&mut context, "inhibit-redisplay", Value::T)
            .expect("ordinary special variable accepts its binding");
    }
    assert_eq!(
        context.variable_value_by_name("inhibit-redisplay"),
        Some(Value::fixnum(7))
    );
}

#[test]
fn foreground_underline_symbol_does_not_become_a_color_name() {
    let attribute = Value::list(vec![
        Value::keyword("color"),
        Value::string("red"),
        Value::keyword("color"),
        Value::symbol("foreground-color"),
    ]);
    let converted = super::super::lisp_value_to_face_attr_resolved(
        LFaceAttr::Underline,
        attribute,
        FaceColorResolver::Standard,
    );
    let Some(crate::face::FaceAttrValue::SpecifiedUnderline { underline, color }) = converted
    else {
        panic!("a color plist remains an explicitly enabled underline");
    };
    assert_eq!(color, None);
    assert_eq!(underline.color, None);

    let attribute = Value::list(vec![
        Value::keyword("color"),
        Value::string("foreground-color"),
    ]);
    let converted = super::super::lisp_value_to_face_attr_resolved(
        LFaceAttr::Underline,
        attribute,
        FaceColorResolver::Standard,
    );
    let Some(crate::face::FaceAttrValue::SpecifiedUnderline { color, .. }) = converted else {
        panic!("a string color remains an explicitly enabled underline");
    };
    assert_eq!(
        color,
        Some(SpecifiedColor::Named("foreground-color".to_owned()))
    );
}

#[test]
fn initial_terminal_faces_hide_creation_colors_and_restore_owners() {
    let mut context = super::super::super::eval::Context::new();
    let frame = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut context);
    let foreground = Value::string("#123456");
    let background = Value::string("#654321");
    let definitions = {
        let owner = context.frames.get_mut(frame).expect("selected frame");
        owner.set_known_parameter(FrameParam::ForegroundColor, foreground);
        owner.set_known_parameter(FrameParam::BackgroundColor, background);
        owner.face_hash_table
    };
    {
        let initial = InitialTerminalFaces::new(&mut context, frame).expect("live frame");
        let owner = initial.context.frames.get(frame).expect("live frame");
        assert_eq!(owner.known_parameter(FrameParam::ForegroundColor), None);
        assert_eq!(owner.known_parameter(FrameParam::BackgroundColor), None);
        assert_ne!(owner.face_hash_table, definitions);
    }
    let owner = context.frames.get(frame).expect("live frame");
    assert_eq!(
        owner.known_parameter(FrameParam::ForegroundColor),
        Some(foreground)
    );
    assert_eq!(
        owner.known_parameter(FrameParam::BackgroundColor),
        Some(background)
    );
    assert_eq!(owner.face_hash_table, definitions);
}

#[test]
fn named_and_anonymous_underlines_retain_identical_ordered_operations() {
    let attribute = Value::list(vec![
        Value::keyword("color"),
        Value::string("#111111"),
        Value::keyword("color"),
        Value::symbol("foreground-color"),
        Value::keyword("color"),
        Value::string("#222222"),
    ]);
    let vector = super::super::make_lisp_face_vector();
    super::super::set_lisp_face_vector_attr(vector, LFaceAttr::Underline, attribute);
    let named = super::super::runtime_face_from_lisp_face_vector("ordered", vector);
    let anonymous = Face::from_plist("ordered", &[Value::keyword("underline"), attribute]);
    assert_eq!(named.color_specifications, anonymous.color_specifications);
    assert_eq!(named.underline, anonymous.underline);
    assert_eq!(
        named
            .color_specifications
            .underline_operations
            .as_deref()
            .map(|operations| operations.len()),
        Some(3)
    );
}
