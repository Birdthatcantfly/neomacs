//! The routed row's single render pass and its refusal rewind.
//!
//! The route produces every planned part ONCE through the real append writer
//! and, when the row turns out not to fit, restores a checkpoint of the row
//! and the walk and leaves the row to the buffer pipeline — GNU's shape, where
//! `display_line` restores the saved display iterator (`SAVE_IT` /
//! `RESTORE_IT`, src/xdisp.c:840-861) and a row's glyphs are produced once.
//! These tests force that refusal with an armed injection (`nextest` runs one
//! test per process) so the rewind is exercised against REAL appended glyphs,
//! both for a single-part row (the whole row is rolled back) and for a
//! face-segmented row (the first segment's glyphs are rolled back before the
//! second segment is produced).
//!
//! The observable is the finished layout: a refused-and-rewound attempt must
//! leave the matrix exactly as the control layout that never refused — no
//! duplicated glyphs, no residual pen advance, no leaked row metrics, no
//! cursor or row-position drift.

use super::*;

/// Point sits in the first line so the classifier refuses that row (rows
/// carrying point keep the buffer pipeline) and the remaining plain rows are
/// genuinely routed.
fn routed_rows_trace(text: &str, face_span: Option<(usize, usize)>) -> BackendLayoutTrace {
    layout_trace_with_buffer_setup(text, 640, 240, |buffer, _, _| {
        if let Some((start, end)) = face_span {
            assert!(buffer.put_text_property(
                start,
                end,
                Value::symbol("face"),
                Value::symbol("bold"),
            ));
        }
        buffer.goto_emacs_byte_pos(neovm_core::buffer::EmacsBytePos::new(1));
    })
}

/// The displayed characters per text row, with the row-end fill space the
/// finalizer appends trimmed off.
fn trimmed_row_texts(trace: &BackendLayoutTrace) -> Vec<String> {
    row_texts(trace)
        .into_iter()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn routed_row_count() -> usize {
    crate::buffer_source::row_route::ROUTED_ROW_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

fn route_rewind_count() -> usize {
    crate::buffer_source::row_route::ROUTE_REWIND_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}

/// A refused single-part row: the whole routed row has already been appended
/// when the refusal fires, so the rewind must roll back every glyph, the pen,
/// the row geometry and the walk position. The pipeline then produces the row
/// from its own source and the finished matrix matches the never-refused
/// control byte for byte.
#[test]
fn routed_row_commit_refusal_rewinds_a_complete_row() {
    let text = "seed\nhello world\nmore\n";
    let control_engagements = routed_row_count();
    let control = routed_rows_trace(text, None);
    let control_engagements = routed_row_count() - control_engagements;
    assert!(
        control_engagements >= 2,
        "the fixture must take the route on both plain rows (got {control_engagements})"
    );
    assert_eq!(
        trimmed_row_texts(&control),
        vec!["seed", "hello world", "more"],
        "fixture sanity: the control layout displays the three lines"
    );

    let rewinds_before = route_rewind_count();
    let injected = {
        let _injection = crate::buffer_source::row_route::RoutedRowRefusalInjection::arm(1);
        routed_rows_trace(text, None)
    };
    assert_eq!(
        route_rewind_count() - rewinds_before,
        1,
        "exactly one routed-row attempt must have rewound"
    );
    assert_eq!(
        injected.matrix_rows, control.matrix_rows,
        "a rewound row must leave the matrix exactly as the never-refused layout"
    );
    assert_eq!(
        injected.points, control.points,
        "the refused attempt's emitted display points must be truncated away"
    );
    assert_eq!(
        injected.output_rows, control.output_rows,
        "published row snapshots must not remember the refused attempt"
    );
    assert_eq!(injected.window_end_pos, control.window_end_pos);
    assert_eq!(injected.window_end_bytepos, control.window_end_bytepos);
}

/// A refused row that is only PARTLY produced: a face span splits the routed
/// row into two text parts, and the refusal fires before the second part, so
/// the first part's glyphs, its vertical metrics and the advanced pen must all
/// be rolled back before the buffer pipeline re-produces the row.
#[test]
fn routed_row_commit_refusal_rewinds_a_partial_row() {
    let text = "seed\nhello world\nmore\n";
    // "hello" is bold, splitting the routed row into two face segments.
    let face_span = Some((5, 10));
    let control = routed_rows_trace(text, face_span);
    assert_eq!(
        trimmed_row_texts(&control),
        vec!["seed", "hello world", "more"],
        "fixture sanity: the segmented control row still displays the whole line"
    );

    let rewinds_before = route_rewind_count();
    let injected = {
        let _injection = crate::buffer_source::row_route::RoutedRowRefusalInjection::arm(1);
        routed_rows_trace(text, face_span)
    };
    assert_eq!(
        route_rewind_count() - rewinds_before,
        1,
        "exactly one routed-row attempt must have rewound"
    );
    assert_eq!(
        injected.matrix_rows, control.matrix_rows,
        "a partially produced row must rewind its first segment completely"
    );
    assert_eq!(
        injected.points, control.points,
        "a partially produced row must truncate its emitted display points"
    );
    assert_eq!(
        injected.output_rows, control.output_rows,
        "published row snapshots must not remember the refused attempt"
    );
    assert_eq!(injected.window_end_pos, control.window_end_pos);
    assert_eq!(injected.window_end_bytepos, control.window_end_bytepos);
}
