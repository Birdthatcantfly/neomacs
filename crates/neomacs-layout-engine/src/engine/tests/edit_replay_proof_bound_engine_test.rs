//! The below-reuse ASCII proof is window-bounded (GNU `try_window_id`'s
//! `GIVE_UP (15)` plus `get_narrowed_len`): a dirty span covering the window
//! start, or longer than the window can display, must not query every dirty
//! character, while ordinary small spans keep the eager proof.
use super::*;
use crate::incremental_layout::lazy_proof_test_support::{self as probes, Guard};

/// Probes observed for one edit form under the default (eager) policy; the
/// harness's own incremental-vs-full comparison inside `run_with_policy`
/// certifies the display output of every arm.
fn proof_counts(point: usize, form: &str) -> probes::Counts {
    let _guard = Guard::set(false);
    let _ = run_with_policy(false, EditSyncMode::Sync, true, &source(), point, &[form]);
    probes::counts()
}

#[test]
fn huge_insert_covering_the_window_start_skips_the_per_char_proof() {
    // Point at the buffer start: the inserted block covers the window start,
    // exactly GNU try_window_id's GIVE_UP (15) condition.
    let counts = proof_counts(0, "(insert (make-string 40000 ?a))");
    assert_eq!(
        counts.char_queries, 0,
        "a span covering the window start must not be proven per-char: {counts:?}"
    );
}

#[test]
fn huge_off_window_insert_is_bounded_by_the_window_not_the_span() {
    // Point past the first lines: the span no longer covers the window
    // start, but 40000 chars exceed the whole narrowed region the window
    // can display, so the proof declines instead of scanning the span.
    let text = source();
    let counts = proof_counts(line_end(&text, 8), "(insert (make-string 40000 ?a))");
    assert_eq!(
        counts.char_queries, 0,
        "a span longer than the window's narrowed region must not be proven per-char: {counts:?}"
    );
}

#[test]
fn small_local_edit_keeps_the_eager_per_char_proof() {
    let text = source();
    let counts = proof_counts(line_end(&text, 8), "(insert \"x\")");
    assert!(
        counts.char_queries > 0,
        "a small local span keeps the eager proof: {counts:?}"
    );
}
