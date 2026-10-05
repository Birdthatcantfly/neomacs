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
    let mut context = crate::test_utils::runtime_startup_context();
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

#[test]
fn printer_settings_gnu_override_scopes() {
    oracle(
        "override_scopes",
        r###"(let ((print-length 99) (print-quoted nil) (print-escape-nonascii nil)) (list (prin1-to-string (make-marker) nil (list (cons 'length 7) (cons 'quoted t) (cons 'unreadable-function (lambda (_o _e) (concat (number-to-string print-length) (if print-quoted "q" "-") (if print-escape-nonascii "e" "-") (if print-unreadable-function "u" "-")))))) (list print-length print-quoted print-escape-nonascii) (condition-case e (prin1-to-string (make-marker) nil (list (cons 'length 7) (cons 'unreadable-function (lambda (_o _e) (error "scope"))))) (error e)) print-length (prin1-to-string (make-marker) nil (list t (cons 'unreadable-function (lambda (_o _e) (concat (if print-length "l" "-") (if print-quoted "q" "-") (if print-charset-text-property "c" "-"))))))))"###,
        include_str!("settings_gnu/override_scopes.expect"),
    );
}

#[test]
fn printer_settings_gnu_callback_buffer() {
    oracle(
        "callback_buffer",
        r###"(with-temp-buffer (let ((origin (current-buffer)) (print-unreadable-function (lambda (_o _e) (set-buffer (get-buffer-create "printer-callback-target")) "Q"))) (list (prin1-to-string (vector (make-marker) (make-marker))) (eq origin (current-buffer)) (condition-case e (let ((print-unreadable-function (lambda (_o _e) (set-buffer (get-buffer-create "printer-callback-target")) (error "changed")))) (prin1-to-string (make-marker))) (error e)) (eq origin (current-buffer)))))"###,
        include_str!("settings_gnu/callback_buffer.expect"),
    );
}

#[test]
fn printer_settings_gnu_buffer_princ() {
    oracle(
        "buffer_princ",
        r###"(with-temp-buffer (rename-buffer "printer buffer") (let ((print-unreadable-function (lambda (_o _e) nil))) (list (prin1-to-string (current-buffer) t) (format "%s" (list (current-buffer))) (format "%S" (list (current-buffer))))))"###,
        include_str!("settings_gnu/buffer_princ.expect"),
    );
}

#[test]
fn printer_settings_gnu_callback_gc_plists() {
    oracle(
        "callback_gc_plists",
        r###"(let* ((tail (list (make-marker) 'after)) (object (propertize "ab" 'charset 'ascii 'payload tail)) (print-unreadable-function (lambda (_o _e) (set-text-properties 0 2 nil object) (garbage-collect) "Q"))) (list (prin1-to-string object) (text-properties-at 0 object)))"###,
        include_str!("settings_gnu/callback_gc_plists.expect"),
    );
}

#[test]
fn printer_settings_gnu_override_watchers() {
    oracle(
        "override_watchers",
        r###"(let* ((print-length 99)
        (print-quoted nil)
        (events nil)
        (overrides (list (cons 'length 3) (cons 'quoted t)
                         (cons 'unreadable-function (lambda (_object _escaping) "kept"))))
        (watcher (lambda (_symbol value operation _where)
                   (when (memq operation '(let unlet)) (push (list operation value) events))
                   (when (eq operation 'let)
                     (setcdr overrides nil)
                     (garbage-collect)))))
   (unwind-protect
       (progn
         (add-variable-watcher 'print-quoted watcher)
         (let ((text (prin1-to-string (make-marker) nil overrides)))
           (list text print-length print-quoted (nreverse events))))
     (remove-variable-watcher 'print-quoted watcher)))"###,
        include_str!("settings_gnu/override_watchers.expect"),
    );
}

#[test]
fn printer_settings_gnu_live_options() {
    oracle(
        "live_options",
        r###"(list
  (let ((print-quoted t)
        (print-unreadable-function (lambda (_o _e) (setq print-quoted nil) "Q")))
    (prin1-to-string (vector (make-marker) '(quote a))))
  (let ((print-length nil)
        (print-unreadable-function (lambda (_o _e) (setq print-length 1) "Q")))
    (prin1-to-string (vector (make-marker) '(1 2) '(3 4))))
  (let ((print-length nil)
        (print-unreadable-function (lambda (_o _e) (setq print-length 1) "Q")))
    (prin1-to-string (list (make-marker) '(1 2) '(3 4))))
  (let ((print-level nil)
        (print-unreadable-function (lambda (_o _e) (setq print-level 1) "Q")))
    (prin1-to-string (vector (make-marker) '(1 2) (vector 3 4))))
  (let ((print-circle nil)
        (print-unreadable-function (lambda (_o _e) (setq print-circle t) "Q")))
    (condition-case e (prin1-to-string (vector (make-marker) '(a))) (error e)))
  (let ((print-circle t)
        (print-unreadable-function (lambda (_o _e) (let ((print-circle nil)) (prin1 1 #'ignore)) "Q")))
    (condition-case e (let ((shared '(a))) (prin1-to-string (vector (make-marker) shared shared))) (error e)))
  (let ((print-length nil)
        (print-unreadable-function (lambda (_o _e) (setq print-length 1) "Q"))
        (h (make-hash-table :test 'eq)))
    (puthash (make-marker) '(1 2) h)
    (puthash 'next '(3 4) h)
    (prin1-to-string h)))"###,
        include_str!("settings_gnu/live_options.expect"),
    );
}

#[test]
fn printer_settings_gnu_callback_string_flags() {
    oracle(
        "callback_string_flags",
        r###"(list
  (let ((print-unreadable-function (lambda (_o _e) (setq print-escape-nonascii nil) (unibyte-string 200))))
    (append (prin1-to-string (make-marker)) nil))
  (let ((print-unreadable-function (lambda (_o _e) (setq print-escape-nonascii t) (unibyte-string 200))))
    (append (prin1-to-string (make-marker)) nil))
  (let ((print-circle nil)
        (print-unreadable-function (lambda (_o _e) (setq print-circle t) "Q")))
    (condition-case e (prin1-to-string (vector (make-marker) 3)) (error e)))
  (let ((print-escape-newlines nil)
        (print-unreadable-function (lambda (_o _e) (setq print-escape-newlines t) "Q")))
    (prin1-to-string (vector (make-marker) "a\nb"))))"###,
        include_str!("settings_gnu/callback_string_flags.expect"),
    );
}

#[test]
fn printer_settings_gnu_shorthand_refresh() {
    oracle(
        "shorthand_refresh",
        r###"(list
  (let ((print-quoted t)
        (print-unreadable-function (lambda (_o _e) (setq print-quoted nil) "Q")))
    (prin1-to-string (vector (list 'quote (make-marker)) '(quote a))))
  (let ((print-quoted t)
        (print-unreadable-function (lambda (_o _e) (setq print-quoted nil) "Q")))
    (prin1-to-string (vector (list '\` (make-marker)) '(quote a))))
  (let ((print-quoted t)
        (print-unreadable-function (lambda (_o _e) (setq print-quoted nil) "Q")))
    (prin1-to-string (vector (list 'quote (vector (make-marker) '(quote a))) '(quote b)))))"###,
        include_str!("settings_gnu/shorthand_refresh.expect"),
    );
}

#[test]
fn printer_settings_gnu_format_source_ownership() {
    oracle(
        "format_source_ownership",
        r###"(list
 (let* ((fmt (copy-sequence "%s:%s:%s")) (calls 0)
        (print-unreadable-function (lambda (_o _e) (setq calls (1+ calls)) (aset fmt (if (= calls 1) 4 7) ?d) "Q")))
   (list (format fmt (make-marker) (make-marker) "tail") fmt calls))
 (let* ((fmt (string-to-unibyte (copy-sequence "%s:%s")))
        (print-unreadable-function (lambda (_o _e) (aset fmt 2 ?X) (aset fmt 4 ?d) "Q")))
   (list (format fmt (make-marker) "tail") fmt))
 (let* ((fmt (copy-sequence "%sé%s"))
        (print-unreadable-function (lambda (_o _e) (aset fmt 4 ?d) (garbage-collect) "Q")))
   (list (format fmt (make-marker) "tail") fmt))
 (let* ((fmt (copy-sequence "%Sé%s"))
        (print-unreadable-function (lambda (_o _e) (aset fmt 4 ?d) (garbage-collect) "Q")))
   (list (format-message fmt (make-marker) "tail") fmt))
 (let* ((fmt (unibyte-string 37 115 128 37 115))
        (print-unreadable-function (lambda (_o _e) (aset fmt 2 255) (aset fmt 4 ?d) "Q")))
   (list (append (format fmt (make-marker) "tail") nil) (append fmt nil)))
 (let* ((fmt (propertize "%S|%s" 'tag 'before))
        (print-unreadable-function (lambda (_o _e) (aset fmt 4 ?d) (set-text-properties 0 5 '(tag after) fmt) "Q")))
   (let ((result (format fmt (make-marker) "tail")))
     (list (substring-no-properties result) (text-properties-at 0 result) (text-properties-at 3 result))))
 (let* ((fmt (copy-sequence "%S:%s"))
        (print-escape-nonascii nil)
        (print-unreadable-function nil)
        (watcher (lambda (_symbol _value operation _where)
                   (when (eq operation 'let)
                     (aset fmt 4 ?d)
                     (setq print-unreadable-function (lambda (_o _e) "W"))))))
   (unwind-protect
       (progn (add-variable-watcher 'print-escape-nonascii watcher)
              (list (format fmt (make-marker) "tail") fmt))
     (remove-variable-watcher 'print-escape-nonascii watcher))))"###,
        include_str!("settings_gnu/format_source_ownership.expect"),
    );
}
