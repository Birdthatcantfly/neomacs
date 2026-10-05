//! GNU 31.1 reader/symbol regression fixtures, refreshed only from GNU.
use crate::test_utils::runtime_startup_eval_one;
use std::path::PathBuf;
fn assert_gnu(name: &str, form: &str, fixture: &str) {
    crate::test_utils::init_test_tracing();
    let expected = if std::env::var("UPDATE_EXPECT").as_deref() == Ok("1") {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("../../tmp")
            .join(format!("symbols-{name}-{}.el", std::process::id()));
        std::fs::write(&script, format!("(prin1 {form})")).unwrap();
        let emacs = std::env::var_os("EMACS").unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap())
                .join(".local/bin/emacs")
                .into_os_string()
        });
        let sandbox = std::env::var_os("NEOVM_LISP_SANDBOX").unwrap_or_else(|| {
            "/home/exec/Projects/github.com/eval-exec/neomacs-main/tmp/v10x/sandbox-run.sh".into()
        });
        let output = std::process::Command::new(sandbox)
            .arg(emacs)
            .args(["-Q", "--batch", "-l"])
            .arg(script)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = format!("OK {}", String::from_utf8(output.stdout).unwrap());
        let path = root
            .join("src/emacs_core/lisp/reader/tests/symbol_reader_gnu")
            .join(format!("{name}.expect"));
        let saved = format!("{text}\n");
        if std::fs::read_to_string(&path).ok().as_deref() != Some(saved.as_str()) {
            std::fs::write(path, saved).unwrap();
        }
        text
    } else {
        fixture.trim_end().to_owned()
    };
    assert_eq!(runtime_startup_eval_one(form), expected, "{name}");
}

#[test]
fn symbols_gnu_identity() {
    assert_gnu(
        "identity",
        r###"(list (intern-soft (make-symbol "car")) (intern-soft 'car) (let ((ob (obarray-make))) (intern-soft (intern "foo" ob))) (let ((ob (obarray-make))) (intern "foo" ob) (intern-soft 'foo ob)))"###,
        include_str!("symbol_reader_gnu/identity.expect"),
    );
}

#[test]
fn symbols_gnu_shorthand_intern() {
    assert_gnu(
        "shorthand_intern",
        r###"(let ((read-symbol-shorthands '(("s-" . "string-")))) (list (intern "s-trim") (intern-soft "s-trim") (let ((ob (obarray-make))) (list (symbol-name (intern "s-probe" ob)) (eq (intern-soft "s-probe" ob) (intern "s-probe" ob)))) (let ((read-symbol-shorthands '(("-" . "x-")))) (intern "-"))))"###,
        include_str!("symbol_reader_gnu/shorthand_intern.expect"),
    );
}

#[test]
fn symbols_gnu_buffer_local_shorthands() {
    assert_gnu(
        "buffer_local_shorthands",
        r###"(with-temp-buffer
       (setq-local read-symbol-shorthands '(("gdj-local-" . "car")))
       (list (intern "gdj-local-") (intern-soft "gdj-local-")
             (car (read-from-string "gdj-local-"))))"###,
        include_str!("symbol_reader_gnu/buffer_local_shorthands.expect"),
    );
}

#[test]
fn symbols_gnu_compiled_intern_soft_identity() {
    assert_gnu(
        "compiled_intern_soft_identity",
        r###"(progn (require 'bytecomp)
      (let ((f (byte-compile (lambda (x) (intern-soft x))))
            (read-symbol-shorthands '(("gdj-compiled-" . "car"))))
        (let ((i 0)) (while (< i 1000) (funcall f "gdj-compiled-") (setq i (1+ i))))
        (list (funcall f (make-symbol "car")) (funcall f 'car)
              (funcall f "gdj-compiled-"))))"###,
        include_str!("symbol_reader_gnu/compiled_intern_soft_identity.expect"),
    );
}

#[test]
fn symbols_gnu_hash_delimiter() {
    assert_gnu(
        "hash_delimiter",
        r###"(list (read-from-string "foo#bar") (read-from-string "(a#'b)") (read-from-string "1#") (read-from-string "#:#") (read-from-string "a#b"))"###,
        include_str!("symbol_reader_gnu/hash_delimiter.expect"),
    );
}

#[test]
fn symbols_gnu_empty_bypass() {
    assert_gnu(
        "empty_bypass",
        r###"(list (mapcar (lambda (s) (let ((pair (read-from-string s))) (list (symbol-name (car pair)) (cdr pair) (eq (car pair) (intern ""))))) '("#_" "#_ " "#_'a" "#_[a]" "#_#_a")) (let ((read-symbol-shorthands '(("" . "gdj-empty-prefix-")))) (mapcar (lambda (source) (let ((value (car (read-from-string source)))) (list (symbol-name value) (multibyte-string-p (symbol-name value))))) '("#_" "#_ " "##" "#:" "#_gdj-bypass-fresh-name"))))"###,
        include_str!("symbol_reader_gnu/empty_bypass.expect"),
    );
}

#[test]
fn symbols_gnu_escape_eof() {
    assert_gnu(
        "escape_eof",
        r###"(mapcar (lambda (s) (condition-case e (read-from-string s) (error e))) '("a\\" "\\" "#:a\\" "#_a\\"))"###,
        include_str!("symbol_reader_gnu/escape_eof.expect"),
    );
}
