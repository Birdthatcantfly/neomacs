//! The single-pass whole-run append (the fit-probe removal).
//!
//! A multi-character run is appended ONCE through the unified push pipeline:
//! the writer itself stops at the row's right edge and reports where it
//! stopped, the producer is reseated at the first unfitting character, and
//! that character's own element arm runs the row's overflow lifecycle. This
//! is GNU's shape — `move_it_in_display_line_to` produces glyphs once and
//! rejects a character once `it->current_x + pixel_width > it->last_visible_x`
//! (src/xdisp.c:10422-10451) — where the former fit probe measured the whole
//! run through a scratch row clone and then rendered it again.
//!
//! The observables of the append's OWN edge stop, pinned here: the rows
//! reassemble the run with nothing lost, duplicated, or invented at the
//! resume seam (ASCII and multibyte), a run that exactly fills the row leaves
//! the next run's first character for the next row (the nothing-drawn
//! refusal), a wide character that would straddle the edge is not split, and
//! truncate mode keeps a single row. 10 cells per row at 88px, the geometry
//! golden stated by the p4.5 family above.

use super::*;

/// 88px frame, char wrap: `truncate-lines = nil`, `word-wrap = nil`.
fn char_wrap_trace(text: &str) -> BackendLayoutTrace {
    layout_trace_with_buffer_setup(text, 88, 240, |buffer, _, _| {
        buffer.set_buffer_local("truncate-lines", Value::NIL);
    })
}

/// A clipped whole-run append stops inside the run and reseats the producer
/// at the first unfitting character, so each continuation row opens with
/// exactly that character — the seam this family exists to pin.
#[test]
fn single_pass_clipped_run_rows_reassemble_the_line() {
    let trace = char_wrap_trace("0123456789abcdefghijKLMNOPQRST\n");
    assert_eq!(
        row_texts(&trace),
        vec!["0123456789\\", "abcdefghij\\", "KLMNOPQRST "],
        "each clipped prefix stays on its row, the marker column ends the full rows, \
         and the rows reassemble the 30-character run"
    );
    assert_eq!(
        row_break_charposes(&trace),
        vec![0, 10, 20],
        "the resume position is the first UNFITTING character, not the last emitted one"
    );
}

/// Multibyte resume arithmetic: the resume byte index is the last emitted
/// character's byte position plus its own UTF-8 length, so a reseeded
/// producer never starts mid-codepoint. Two-byte Greek across three rows.
#[test]
fn single_pass_clipped_multibyte_run_resumes_on_char_boundaries() {
    let line = "αβγδεζηθικλμνξοπρστυφχψω";
    let trace = char_wrap_trace(&format!("{line}\n"));
    let rows = row_texts(&trace);
    let joined: String = rows.concat().chars().filter(|&ch| ch != '\\').collect();
    assert_eq!(
        joined.trim_end(),
        line,
        "the rows must reassemble the 24-character run with no codepoint lost or split"
    );
    assert_eq!(
        row_break_charposes(&trace),
        vec![0, 10, 20],
        "breaks land on 10-character boundaries of CHARPOS, not bytes"
    );
}

/// A run that EXACTLY fills the row leaves the next run refused with nothing
/// drawn (its first glyph starts at the full edge): the refusal drops to
/// character granularity, the row ends there, and the next row opens with
/// the refused character. Five double-width characters fill the 10-cell row.
#[test]
fn single_pass_run_after_an_exact_fill_opens_the_next_row() {
    let trace = char_wrap_trace("汉汉汉汉汉abcdefg\n");
    assert_eq!(
        row_texts(&trace),
        vec!["汉 汉 汉 汉 汉 \\", "abcdefg "],
        "the exactly-filling wide run stays whole (a padding cell follows each \
         wide glyph) and the ASCII run that followed it opens the continuation row"
    );
    assert_eq!(row_break_charposes(&trace), vec![0, 5]);
}

/// A wide character that would straddle the right edge is refused whole —
/// its left cell is never drawn (GNU leaves the partial cell to the
/// continuation marker) — and it opens the next row.
#[test]
fn single_pass_wide_char_straddling_the_edge_is_not_split() {
    let trace = char_wrap_trace("012345678汉x\n");
    assert_eq!(
        row_texts(&trace),
        vec!["012345678 \\", "汉 x "],
        "nine single cells fill the row, the partial cell is padded, and the \
         double-width character moves down whole"
    );
    assert_eq!(row_break_charposes(&trace), vec![0, 9]);
}

/// A clip whose KEPT prefix ends with a wide character: the last emitted
/// slot is the wide glyph (its continuation cell included), so the resume
/// arithmetic must add the character's own UTF-8 length — not a slot or cell
/// count — to land on the next character. The refused wide character then
/// opens the continuation row whole, exactly like the straddling case.
#[test]
fn single_pass_clip_after_a_kept_wide_char_resumes_at_the_next_char() {
    let trace = char_wrap_trace("abcdefg汉汉x\n");
    let rows = row_texts(&trace);
    assert!(
        rows.len() >= 2,
        "the refused second wide character opens a continuation row: {rows:?}"
    );
    assert_eq!(
        row_break_charposes(&trace),
        vec![0, 8],
        "seven ASCII characters plus the first wide one fill nine cells; the \
         second wide one is the first unfitting CHARACTER"
    );
    assert!(
        rows[1].starts_with('汉'),
        "the continuation row opens with the refused wide character: {rows:?}"
    );
    assert!(
        rows[0].starts_with("abcdefg汉"),
        "the kept prefix ends with the wide character, not before it: {rows:?}"
    );
}

/// Truncate mode: the clipped run ends the row with no continuation row —
/// the rest of the line exists in the buffer but not in the window.
#[test]
fn single_pass_clipped_truncate_mode_keeps_one_row() {
    let trace =
        layout_trace_with_buffer_setup("0123456789abcdefghij\n", 88, 240, |buffer, _, _| {
            buffer.set_buffer_local("truncate-lines", Value::T);
        });
    let rows = row_texts(&trace);
    assert_eq!(
        rows,
        vec!["0123456789$"],
        "truncation keeps the fitting prefix on a single row, the '$' marker \
         in the last column, and drops the rest"
    );
}

/// The cursor slot for point inside a clipped run comes from the append's own
/// per-character slots (the zip this path reuses), so point beyond the first
/// row's edge lands on the continuation row the reseat produced. 79 cells per
/// row at 640px, the p4.5 golden.
#[test]
fn single_pass_cursor_tracks_point_across_the_clip_seam() {
    let text = format!("{}\n", "a".repeat(200));
    // Byte 79 is the first character the first row refused.
    assert_eq!(
        char_wrap_cursor_at(&text, 78),
        (0, 78),
        "last fitting character"
    );
    assert_eq!(
        char_wrap_cursor_at(&text, 79),
        (1, 0),
        "the refused character opens the continuation row the reseat produced"
    );
    assert_eq!(
        char_wrap_cursor_at(&text, 160),
        (2, 2),
        "the second clip seam resumes at charpos 158 (79 + 79)"
    );
}
