//! Request-local buffer capture reuse preserves owned freshness inputs.
use crate::buffer::BufferId;
use crate::emacs_core::{Context, Value};
use crate::window::{FrameId, WindowId};

fn setup() -> (Context, BufferId, FrameId, WindowId) {
    let mut eval = Context::new();
    let live = eval.buffer_manager().current_buffer().unwrap().id();
    let frame = eval
        .frame_manager_mut()
        .create_frame("attempt-reuse", 800, 600, live);
    let window = eval.frame_manager().get(frame).unwrap().selected_window;
    (eval, live, frame, window)
}

#[test]
fn same_buffer_attempt_captures_fontifier_identity_once() {
    let (eval, live, frame, window) = setup();
    // Initialize the existing symbol table and all first-read metadata before
    // isolating this request's capture budget. This is not an Ir measurement.
    eval.window_layout_attempt_freshness(frame, window, live)
        .unwrap();
    crate::emacs_core::intern::reset_intern_calls();
    let captured = eval
        .window_layout_attempt_freshness(frame, window, live)
        .unwrap();
    let names = crate::emacs_core::intern::intern_call_names();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "fontification-functions")
            .count(),
        1
    );
    assert_eq!(captured.live_buffer, captured.source_buffer);
    assert_eq!(
        captured.live_buffer,
        eval.buffer_layout_inputs(live).unwrap()
    );
}

#[test]
fn same_buffer_attempt_owns_mutable_prefix_table_and_fontifier_inputs() {
    let (mut eval, live, frame, window) = setup();
    eval.eval_str(
        r#"(progn
          (setq ttykey-scroll-capture-called 0
                ttykey-scroll-prefix (copy-sequence "ab")
                ttykey-scroll-glyph (vector ?A)
                buffer-display-table (make-char-table 'display-table nil)
                line-prefix ttykey-scroll-prefix
                wrap-prefix ttykey-scroll-prefix
                fontification-functions nil)
          (put-text-property
           0 1 'display
           '(when (progn
                    (setq ttykey-scroll-capture-called
                          (1+ ttykey-scroll-capture-called))
                    t) . "x")
           ttykey-scroll-prefix)
          (aset buffer-display-table ?a ttykey-scroll-glyph))"#,
    )
    .unwrap();
    let before = eval
        .window_layout_attempt_freshness(frame, window, live)
        .unwrap();
    assert_eq!(before.live_buffer, before.source_buffer);
    let old_debug = format!("{:?}", before);
    eval.eval_str(
        r#"(progn
          (aset ttykey-scroll-prefix 0 ?z)
          (aset ttykey-scroll-glyph 0 ?B)
          (setq fontification-functions (list 'ignore)))"#,
    )
    .unwrap();
    let after = eval
        .window_layout_attempt_freshness(frame, window, live)
        .unwrap();
    assert_eq!(after.live_buffer, after.source_buffer);
    // Both clone members are immutable owned snapshots; mutating the original
    // Lisp objects must not retroactively change either captured member.
    assert_eq!(format!("{:?}", before), old_debug);
    assert_ne!(before.live_buffer.prefixes, after.live_buffer.prefixes);
    assert_ne!(
        before.live_buffer.display_table,
        after.live_buffer.display_table
    );
    assert_ne!(
        before.live_buffer.fontification_functions,
        after.live_buffer.fontification_functions
    );
    assert_eq!(
        before.live_buffer.modified_tick,
        after.live_buffer.modified_tick
    );
    assert_eq!(
        before.live_buffer.chars_modified_tick,
        after.live_buffer.chars_modified_tick
    );
    assert_eq!(
        before.live_buffer.props_modified_tick,
        after.live_buffer.props_modified_tick
    );
    assert_eq!(
        before.live_buffer.overlay_modified_tick,
        after.live_buffer.overlay_modified_tick
    );
    // Metadata capture must not evaluate the embedded display condition.
    assert_eq!(
        eval.eval_str("ttykey-scroll-capture-called").unwrap(),
        Value::fixnum(0)
    );
}

#[test]
fn distinct_buffer_attempt_keeps_two_independent_projections() {
    let (mut eval, live, frame, window) = setup();
    let source = eval
        .buffer_manager_mut()
        .create_buffer("*ttykey-scroll-source*");
    eval.eval_str(
        r#"(progn
          (setq line-prefix "live" fontification-functions nil)
          (set-buffer "*ttykey-scroll-source*")
          (setq line-prefix "source" fontification-functions (list 'ignore)))"#,
    )
    .unwrap();
    eval.window_layout_attempt_freshness(frame, window, source)
        .unwrap();
    crate::emacs_core::intern::reset_intern_calls();
    let captured = eval
        .window_layout_attempt_freshness(frame, window, source)
        .unwrap();
    let names = crate::emacs_core::intern::intern_call_names();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "fontification-functions")
            .count(),
        2
    );
    assert_eq!(captured.live_buffer.id, live);
    assert_eq!(captured.source_buffer.id, source);
    assert_ne!(
        captured.live_buffer.prefixes,
        captured.source_buffer.prefixes
    );
    assert_ne!(
        captured.live_buffer.fontification_functions,
        captured.source_buffer.fontification_functions
    );
    assert_eq!(
        captured.live_buffer,
        eval.buffer_layout_inputs(live).unwrap()
    );
    assert_eq!(
        captured.source_buffer,
        eval.buffer_layout_inputs(source).unwrap()
    );
}
