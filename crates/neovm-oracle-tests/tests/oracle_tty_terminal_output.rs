//! What GNU Emacs 31.1 writes to a text terminal besides the cells, observed
//! on the byte stream of a test-owned PTY.
//!
//! Refresh expectations only from GNU with NEOVM_ORACLE_MODE=refresh
//! UPDATE_EXPECT=1, running the test driver in the required PID sandbox.
//! The forms open a 120x40 terminal frame on the PTY, as the ttykey
//! differential scenes do; the terminal's byte stream is a redisplay
//! observation like any other (src/term.c decides it), so it is part of the
//! expectation and never written by hand.

#![cfg(unix)]

#[path = "../src/common.rs"]
mod common;

#[path = "../src/tty_test_terminal.rs"]
mod tty_test_terminal;

use common::return_if_neovm_enable_oracle_proptest_not_set;
use tty_test_terminal::TestTerminal;

/// A terminal frame on the test PTY named by `TERMINAL_OUTPUT_TEST_TTY`.
const TERMINAL_SUPPORT: &str = r##"
  (setq redisplay-skip-initial-frame nil inhibit-redisplay nil)
  (setq to-frame
    (make-terminal-frame
      (list (cons 'tty (getenv "TERMINAL_OUTPUT_TEST_TTY"))
            '(tty-type . "screen-256color"))))
  (select-frame to-frame)
  (switch-to-buffer (get-buffer-create "to-cursor"))
  (fundamental-mode)
  (dotimes (i 60) (insert (format "line %02d\n" i)))
"##;

/// GNU's term.c has no DECSCUSR (`CSI Ps SP q`): on a text terminal
/// `cursor-type' is a window-system property, and the cursor is whatever the
/// terminal is configured to show, through every redisplay and cursor motion
/// of a session.  The ttykey differential scenes recorded GNU's cursor as the
/// terminal default at all 115 checkpoints while this port wrote a steady
/// block (`CSI 2 SP q`) on every frame.
#[test]
fn tty_default_box_cursor_writes_no_cursor_style_control() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let form = format!(
        r##"(progn {TERMINAL_SUPPORT}
  (goto-char (point-min))
  (redisplay t)
  (forward-line 5)
  (redisplay t)
  (goto-char (point-max))
  (redisplay t)
  (forward-line -3)
  (end-of-line)
  (redisplay t)
  (list (window-start) (posn-x-y (posn-at-point))))"##
    );
    common::assert_oracle_parity_with_terminal_output_expect(
        &form,
        || {
            let terminal = TestTerminal::with_size(40, 120);
            let env = vec![("TERMINAL_OUTPUT_TEST_TTY".to_owned(), terminal.name.clone())];
            (terminal, env)
        },
        |terminal| {
            format!(
                "cursor-style {:?}",
                TestTerminal::cursor_style_controls(&terminal.finish_with_output())
            )
        },
        expect_test::expect![[r#""OK (329 (7 . 16))" cursor-style []"#]],
    );
}
