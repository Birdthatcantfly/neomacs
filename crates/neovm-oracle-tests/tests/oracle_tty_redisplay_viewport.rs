//! Observable viewport decisions of GNU Emacs 31.1 redisplay on a text
//! terminal: the window start a redisplay keeps or moves, and what the
//! window change functions observe when they run.
//!
//! Refresh expectations only from GNU with NEOVM_ORACLE_MODE=refresh
//! UPDATE_EXPECT=1, running the test driver in the required PID sandbox.
//! The forms open a 120x40 terminal frame on a test-owned PTY so the window
//! has the 39 body rows of the ttykey differential scenes these came from.
//!
//! GNU facts the forms rely on: `set-window-start' clears
//! `w->window_end_valid' for every call, NOFORCE or not (src/window.c:1970),
//! `set_window_buffer' zeroes `w->window_end_pos' (src/window.c:4360), and
//! `redisplay_window' keeps a start point is visible from through its "same
//! window start" branch (src/xdisp.c:20972-21060) before it ever recenters.
//! `run_window_change_functions' runs from `redisplay_internal' before any
//! window is laid out or hscrolled (src/xdisp.c:17563, :17942).

#![cfg(unix)]

#[path = "../src/common.rs"]
mod common;

#[path = "../src/tty_test_terminal.rs"]
mod tty_test_terminal;

use common::return_if_neovm_enable_oracle_proptest_not_set;
use tty_test_terminal::TestTerminal;

/// The ttykey semantic fixture's buffer: 1000 equal lines, the window
/// started at line 499 with NOFORCE, point at the end of line 509.
const VIEWPORT_SUPPORT: &str = r##"
  (setq redisplay-skip-initial-frame nil inhibit-redisplay nil)
  (defvar vp-long nil)
  (defvar vp-seen nil)
  (defun vp-line (n)
    (save-excursion (goto-char (point-min)) (forward-line n) (point)))
  (defun vp-fill (name)
    (switch-to-buffer (get-buffer-create name))
    (fundamental-mode)
    (setq mode-line-format nil header-line-format nil)
    (dotimes (i 1000)
      (insert (format "(message \"line %04d\")" i))
      (when vp-long (insert (make-string 180 ?x)))
      (insert "\n"))
    (goto-char (vp-line 499))
    (set-window-start (selected-window) (point) t)
    (goto-char (vp-line 509))
    (end-of-line))
  (defun vp-state ()
    (list (window-start) (posn-x-y (posn-at-point)) (window-hscroll)))
  (setq vp-frame
    (make-terminal-frame
      (list (cons 'tty (getenv "VIEWPORT_TEST_TTY")) '(tty-type . "screen-256color"))))
  (select-frame vp-frame)
"##;

/// The ttykey `eob' scene: a buffer shown at an earlier page is killed and
/// recreated, its window start set near EOB with NOFORCE and point at EOB.
/// GNU displays that start (point is visible from it, eight rows down) and
/// the size-change function run for the buffer switch observes it before
/// any layout.
#[test]
fn tty_viewport_noforce_start_near_eob_is_kept_across_a_buffer_switch() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let terminal = TestTerminal::with_size(40, 120);
    let form = format!(
        r##"(progn {VIEWPORT_SUPPORT}
  (vp-fill "vp-prelude")
  (redisplay t)
  (add-hook 'window-size-change-functions
            (lambda (_frame) (push (list 'size (window-start) (point)) vp-seen)))
  (kill-buffer "vp-prelude")
  (vp-fill "vp-eob")
  (goto-char (point-max))
  (forward-line -8)
  (set-window-start (selected-window) (point) t)
  (goto-char (point-max))
  (setq vp-before (window-start))
  (redisplay t)
  (list vp-before (vp-state) (window-end nil t) (nreverse vp-seen)))"##
    );
    common::assert_oracle_parity_with_env_expect(
        &form,
        &[("VIEWPORT_TEST_TTY", &terminal.name)],
        expect_test::expect![[r#""OK (21825 (21825 (0 . 8) 0) 22001 ((size 21825 22001)))""#]],
    );
    terminal.finish();
}

/// Same buffer, no buffer switch: a NOFORCE start below the page the
/// window showed, with point visible from it, is kept as well -- the old
/// page's end is not evidence about the new start.
#[test]
fn tty_viewport_noforce_start_below_the_shown_page_is_kept_in_the_same_buffer() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let terminal = TestTerminal::with_size(40, 120);
    let form = format!(
        r##"(progn {VIEWPORT_SUPPORT}
  (vp-fill "vp-same")
  (redisplay t)
  (setq vp-first (vp-state))
  (set-window-start (selected-window) (vp-line 900) t)
  (goto-char (vp-line 905))
  (end-of-line)
  (redisplay t)
  (setq vp-second (vp-state))
  (set-window-start (selected-window) (vp-line 300) t)
  (goto-char (vp-line 337))
  (redisplay t)
  (list vp-first vp-second (vp-state)))"##
    );
    common::assert_oracle_parity_with_env_expect(
        &form,
        &[("VIEWPORT_TEST_TTY", &terminal.name)],
        expect_test::expect![[
            r#""OK ((10979 (21 . 10) 0) (19801 (21 . 5) 0) (6601 (0 . 37) 0))""#
        ]],
    );
    terminal.finish();
}

/// The ttykey `truncate' scene: a fresh `truncate-lines' buffer whose point
/// sits past the right edge.  The size-change function run for the buffer
/// switch observes `window-hscroll' 0: GNU runs it before the auto-hscroll
/// of the same redisplay, which then settles at 85.
#[test]
fn tty_size_change_functions_observe_the_window_before_its_auto_hscroll() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let terminal = TestTerminal::with_size(40, 120);
    let form = format!(
        r##"(progn {VIEWPORT_SUPPORT}
  (setq vp-long t)
  (vp-fill "vp-prelude")
  (redisplay t)
  (add-hook 'window-size-change-functions
            (lambda (_frame) (push (list 'size (window-hscroll) (window-start)) vp-seen)))
  (kill-buffer "vp-prelude")
  (vp-fill "vp-truncate")
  (setq-local truncate-lines t)
  (redisplay t)
  (list (nreverse vp-seen) (vp-state)))"##
    );
    common::assert_oracle_parity_with_env_expect(
        &form,
        &[("VIEWPORT_TEST_TTY", &terminal.name)],
        expect_test::expect![[r#""OK (((size 0 100799)) (100799 (116 . 10) 85))""#]],
    );
    terminal.finish();
}

/// The ttykey `truncate' scene's buffer with point kept inside the first 119
/// columns, so nothing hscrolls and every row is right-truncated.  GNU's
/// `display_line` reseats a truncated row past the rest of its line before
/// recording the row's end (src/xdisp.c:26695, :26736), so `window-end' --
/// `Z - w->window_end_pos' from the last row -- is the following line's
/// first position, not the position after the last drawn glyph.  In
/// `--batch' UPDATE never runs the iterator (window.c:1861), so both forms
/// read the recorded end.
#[test]
fn tty_window_end_after_a_truncated_last_row_is_the_next_line_start() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let terminal = TestTerminal::with_size(40, 120);
    let form = format!(
        r##"(progn {VIEWPORT_SUPPORT}
  (setq vp-long t)
  (vp-fill "vp-truncate")
  (setq-local truncate-lines t)
  (goto-char (+ (vp-line 509) 10))
  (redisplay t)
  (setq vp-first (list (window-end) (window-end nil t) (window-hscroll)))
  (goto-char (vp-line 537))
  (redisplay t)
  (list vp-first (list (window-end) (window-end nil t) (window-hscroll) (window-start))))"##
    );
    common::assert_oracle_parity_with_env_expect(
        &form,
        &[("VIEWPORT_TEST_TTY", &terminal.name)],
        expect_test::expect![[r#""OK ((108677 108677 0) (108677 108677 0 100799))""#]],
    );
    terminal.finish();
}
