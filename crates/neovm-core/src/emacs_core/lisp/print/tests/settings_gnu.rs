//! GNU 31.1 printer settings fixtures, refreshed only from GNU with UPDATE_EXPECT=1.
use std::path::PathBuf;
fn oracle(name: &str, form: &str, fixture: &str) {
    crate::test_utils::init_test_tracing();
    let expected = if std::env::var("UPDATE_EXPECT").as_deref() == Ok("1") {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let input = root
            .join("../../tmp")
            .join(format!("settings-{name}-{}.el", std::process::id()));
        std::fs::write(&input, format!("(prin1 {form})\n")).expect("GNU probe");
        let emacs = std::env::var_os("EMACS").unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME"))
                .join(".local/bin/emacs")
                .into_os_string()
        });
        let output = std::process::Command::new(emacs)
            .args(["-Q", "--batch", "-l"])
            .arg(input)
            .output()
            .expect("GNU oracle");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = String::from_utf8(output.stdout).expect("UTF-8 oracle");
        std::fs::write(
            root.join("src/emacs_core/lisp/print/tests/settings_gnu")
                .join(format!("{name}.expect")),
            &expected,
        )
        .expect("GNU fixture");
        expected
    } else {
        fixture.to_owned()
    };
    let mut context = crate::emacs_core::Context::new();
    let actual = crate::emacs_core::format_eval_result(&context.eval_str(form));
    assert_eq!(actual, format!("OK {}", expected.trim_end()), "{name}");
}

#[test]
fn printer_settings_gnu_quoted() {
    oracle(
        "quoted",
        r###"(list (let ((print-quoted nil)) (prin1-to-string '('a #'b `(c ,d)))) (prin1-to-string '(quote x) nil '((quoted . nil))))"###,
        include_str!("settings_gnu/quoted.expect"),
    );
}

#[test]
fn printer_settings_gnu_nbsp() {
    oracle(
        "nbsp",
        r###"(list (append (prin1-to-string (intern "a b")) nil) (eq (read (prin1-to-string (intern "a b"))) (intern "a b")))"###,
        include_str!("settings_gnu/nbsp.expect"),
    );
}

#[test]
fn printer_settings_gnu_hash_princ() {
    oracle(
        "hash_princ",
        r###"(let ((h (make-hash-table :test 'equal))) (puthash "k" "x" h) (list (format "%s" h) (format "%s" (list h)) (with-output-to-string (princ h)) (prin1-to-string h t)))"###,
        include_str!("settings_gnu/hash_princ.expect"),
    );
}

#[test]
fn printer_settings_gnu_unreadable() {
    oracle(
        "unreadable",
        r###"(list (let ((print-unreadable-function (lambda (_o _e) "Q"))) (prin1-to-string (list 1 (make-marker) (current-buffer) (obarray-make)))) (let ((print-unreadable-function (lambda (_o _e) t))) (prin1-to-string (make-marker))) (let ((print-unreadable-function (lambda (_o e) (if e "escaped" "bare")))) (list (prin1-to-string (make-marker)) (prin1-to-string (make-marker) t))) (prin1-to-string (make-marker) nil (list (cons 'unreadable-function (lambda (_o _e) "override")))))"###,
        include_str!("settings_gnu/unreadable.expect"),
    );
}

#[test]
fn printer_settings_gnu_charset() {
    oracle(
        "charset",
        r###"(progn (set-charset-priority 'unicode) (list print-charset-text-property (prin1-to-string (propertize "a" 'charset 'ascii)) (prin1-to-string (propertize "a" 'charset 'ascii 'foo 'bar)) (let ((print-charset-text-property nil)) (prin1-to-string (propertize "é" 'charset 'ascii))) (let ((print-charset-text-property t)) (prin1-to-string (propertize "a" 'charset 'ascii))) (let ((print-charset-text-property 'default)) (prin1-to-string (propertize "é" 'charset 'ascii))) (let ((print-charset-text-property 'default)) (prin1-to-string (propertize "é" 'charset 'unicode)))))"###,
        include_str!("settings_gnu/charset.expect"),
    );
}

#[test]
fn printer_settings_gnu_overrides() {
    oracle(
        "overrides",
        r###"(list (prin1-to-string ?a nil '((integers-as-characters . t))) (condition-case e (prin1-to-string 1 nil '((bogus . 1))) (error e)) (condition-case e (prin1-to-string 1 nil '((length . 1) . 3)) (error e)) (condition-case e (prin1-to-string 1 nil '((3 . 1))) (error e)) (condition-case e (prin1-to-string 1 nil (list (cons (make-symbol "length") 1))) (error e)))"###,
        include_str!("settings_gnu/overrides.expect"),
    );
}

#[test]
fn printer_settings_gnu_unreadable_scopes() {
    oracle(
        "unreadable_scopes",
        r###"(let* ((v (vector (make-marker) (make-marker))) (print-unreadable-function (lambda (o _e) (aset v 1 'changed) (concat (if print-unreadable-function "wrong" "nil") (let (chars) (prin1 o (lambda (c) (push c chars))) (apply #'string (nreverse chars))))))) (list (prin1-to-string v) (condition-case e (let ((print-unreadable-function (lambda (_o _e) (error "callback")))) (prin1-to-string (make-marker))) (error e)) (functionp print-unreadable-function)))"###,
        include_str!("settings_gnu/unreadable_scopes.expect"),
    );
}

#[test]
fn printer_settings_gnu_unreadable_fallback() {
    oracle(
        "unreadable_fallback",
        r###"(list (let ((print-unreadable-function (lambda (_o _e) nil))) (prin1-to-string (make-marker))) (let ((print-unreadable-function 'unbound-printer)) (prin1-to-string (make-marker))) (prin1-to-string (make-marker) nil (list (cons 'unreadeable-function (lambda (_o _e) "alias")))) (let ((print-unreadable-function (lambda (_o e) (if e "yes" "no")))) (list (format "%S" (list (make-marker))) (format "%s" (list (make-marker))) (with-output-to-string (princ (make-marker))))))"###,
        include_str!("settings_gnu/unreadable_fallback.expect"),
    );
}
