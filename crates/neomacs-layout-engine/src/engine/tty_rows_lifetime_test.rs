//! Actual canonical TTY producers through the typed policy entry. These tests
//! must produce their own witnesses; no cached-query hit or synthetic seal.
use super::*;
use crate::engine::{RowsQueryAdmission, RowsQueryPolicy};
use neovm_core::emacs_core::{
    display_host::{DisplayHost, InertRowsHostAuthority},
    eval::GuiFrameHostRequest,
    terminal::pure,
};
use std::num::NonZeroUsize;

struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        pure::reset_terminal_host();
        pure::reset_terminal_runtime();
    }
}

// All closed-query image/font accessors inherit their constant absent default.
// Frame operations are outside this numeric query and must never be probed.
struct InertTtyRowsHost;
impl DisplayHost for InertTtyRowsHost {
    fn realize_gui_frame(&mut self, _: GuiFrameHostRequest) -> Result<(), String> {
        panic!("a TTY numeric query must not realize a GUI frame")
    }
    fn resize_gui_frame(&mut self, _: GuiFrameHostRequest) -> Result<(), String> {
        panic!("a TTY numeric query must not resize a GUI frame")
    }
}

fn terminal_policy() -> RowsQueryPolicy {
    // SAFETY: InertTtyRowsHost is a stateless concrete type whose image/font
    // accessors inherit constant None defaults. They cannot call Lisp,
    // collect, reenter, destroy owners or expose mutable external geometry.
    // Its only overridden methods are outside this closed numeric TTY query.
    let host = unsafe { InertRowsHostAuthority::for_type::<InertTtyRowsHost>() };
    // Stage3b observes eligibility only; this does not enable cache hits.
    RowsQueryPolicy::terminal(RowsQueryAdmission::Off, host)
}

fn rows() -> WindowLayoutQueryScope {
    WindowLayoutQueryScope::Rows {
        start: LispCharPos1::ONE,
        count: NonZeroUsize::new(4).unwrap(),
    }
}

fn fixture() -> (Context, FrameId, WindowId, BufferId, RestoreTerminal) {
    let mut context = Context::new();
    let restore = RestoreTerminal;
    context.set_gc_threshold(usize::MAX);
    context.gc_collect_exact();
    let config: pure::TerminalRuntimeConfig = pure::TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_owned()),
        neomacs_display_protocol::tty_capabilities::TtyAttributeCapabilities::full_with_color_cells(
            256,
        ),
    )
    .into();
    pure::configure_terminal_runtime(config);
    let source = context.buffer_manager().current_buffer().unwrap().id();
    context
        .buffer_manager_mut()
        .get_mut(source)
        .unwrap()
        .insert(&"short tty numeric row\n".repeat(12));
    let frame = context
        .frame_manager_mut()
        .create_frame("actual-tty-rows", 120, 40, source);
    assert!(context.frame_manager_mut().select_frame(frame));
    assert!(context.frame_is_tty_face_target(frame));
    let window = context.frame_manager().get(frame).unwrap().selected_window;
    context
        .eval_str(
            r#"(setq mode-line-format nil header-line-format nil tab-line-format nil
      fontification-functions (list 'ignore) glyphless-char-display nil
      buffer-display-table nil standard-display-table nil char-width-table nil)
      (put-text-property (point-min) (point-max) 'fontified t)
      (goto-char 1) (set-window-start nil 1 t)"#,
        )
        .unwrap();
    context.display_host = Some(Box::new(InertTtyRowsHost));
    context.sync_runtime_faces_for_frame(frame);
    context.setup_thread_locals();
    let policy = terminal_policy();
    let super::super::rows_query_policy::RowsQueryEnvironment::Terminal(host) =
        policy.environment()
    else {
        unreachable!("the fixture requests the explicit terminal policy")
    };
    context
        .begin_tty_rows_observation(frame, Some(&host))
        .expect("actual warm termcap/default and inert host must be admitted before capture");
    (context, frame, window, source, restore)
}

fn query(
    context: &mut Context,
    engine: &mut LayoutEngine,
    frame: FrameId,
    window: WindowId,
    policy: &RowsQueryPolicy,
) -> (WindowLayoutQuery, Vec<Report>) {
    let mut reports = Vec::new();
    let mut sink = |report| reports.push(report);
    let query = engine
        .query_window_layout_observed_with_policy(
            context,
            frame,
            window,
            rows(),
            policy,
            Some(&mut sink),
        )
        .expect("the actual canonical TTY Rows producer must converge");
    drop(sink);
    let geometry = query
        .geometry()
        .expect("canonical TTY query carries geometry");
    assert!(!geometry.rows.is_empty());
    assert!(geometry.point_count() > 0);
    (query, reports)
}

#[test]
fn tty_rows_lifetime_actual_warm_canonical_producer_certifies_and_drops_source_lease() {
    let (mut context, frame, window, source, _restore) = fixture();
    let mut engine = LayoutEngine::new_without_font_metrics();
    let (query, reports) = query(&mut context, &mut engine, frame, window, &terminal_policy());
    let geometry = query.geometry().unwrap();
    assert_eq!(
        reports,
        vec![Report::Certified {
            rows: geometry.rows.len(),
            points: geometry.point_count(),
        }],
        "actual normalization, NoDemand, warm face entry, source and dependency proof must all close"
    );
    // The real completion must drop its opaque Ref before the test sink/return.
    context
        .buffer_manager_mut()
        .get_mut(source)
        .unwrap()
        .insert("after actual TTY proof");
    context.gc_collect_exact();
}

#[test]
fn tty_rows_lifetime_actual_reached_cold_text_face_callback_refuses() {
    let (mut context, frame, window, _, _restore) = fixture();
    context
        .eval_str(
            r#"(setq tty-rows-cold-calls 0 tty-defined-color-alist nil)
      (fset 'tty-color-desc (lambda (&rest ignored)
        (setq tty-rows-cold-calls (1+ tty-rows-cold-calls))
        (list "rows-cold" 4 12345 23456 34567)))
      (put-text-property 1 8 'face '(:foreground "rows-cold"))"#,
        )
        .unwrap();
    let mut engine = LayoutEngine::new_without_font_metrics();
    let (_, reports) = query(&mut context, &mut engine, frame, window, &terminal_policy());
    assert!(
        context
            .eval_str("tty-rows-cold-calls")
            .unwrap()
            .as_int()
            .unwrap()
            > 0,
        "the canonical iterator must actually reach this cold text face"
    );
    assert_eq!(reports, vec![Report::Refused(Refusal::ColdFaceEntry)]);
}

#[test]
fn tty_rows_lifetime_actual_cold_color_callback_gc_cannot_certify() {
    let (mut context, frame, window, _, _restore) = fixture();
    context
        .eval_str(
            r#"(setq tty-rows-cold-calls 0 tty-rows-color-gc-done nil tty-defined-color-alist nil)
      (fset 'tty-color-desc (lambda (&rest ignored)
        (setq tty-rows-cold-calls (1+ tty-rows-cold-calls))
        (if tty-rows-color-gc-done nil
          (garbage-collect) (setq tty-rows-color-gc-done t))
        (list "rows-cold-gc" 4 12345 23456 34567)))
      (put-text-property 1 8 'face '(:foreground "rows-cold-gc"))"#,
        )
        .unwrap();
    let mut engine = LayoutEngine::new_without_font_metrics();
    let (_, reports) = query(&mut context, &mut engine, frame, window, &terminal_policy());
    assert!(
        context
            .eval_str("tty-rows-color-gc-done")
            .unwrap()
            .is_truthy()
    );
    assert!(
        context
            .eval_str("tty-rows-cold-calls")
            .unwrap()
            .as_int()
            .unwrap()
            > 0
    );
    assert_eq!(
        reports,
        vec![Report::Refused(Refusal::ColdFaceEntry)],
        "actual cold entry refuses before any old dependency/source read after callback GC"
    );
}

#[test]
fn tty_rows_lifetime_default_disabled_entry_keeps_legacy_tty_refusal() {
    let (mut context, frame, window, _, _restore) = fixture();
    // Remove the inert host only to expose the legacy frame-class refusal;
    // otherwise legacy begin correctly refuses the host first.
    context.display_host = None;
    let mut engine = LayoutEngine::new_without_font_metrics();
    let (_, reports) = query(
        &mut context,
        &mut engine,
        frame,
        window,
        &RowsQueryPolicy::disabled(),
    );
    assert_eq!(
        reports,
        vec![Report::Refused(Refusal::Core(
            RowsObservationRefusal::TtyOrUnknownFrame,
        ))]
    );
}
