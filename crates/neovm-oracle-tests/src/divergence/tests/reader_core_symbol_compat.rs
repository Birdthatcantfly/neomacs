//! Core-symbol reader fast lookup: GNU lread.c:4188-4255,4858-4922.
//! Removal cases run only in editor subprocesses, never in the Rust test heap.
use crate::common::{assert_oracle_parity_expect, return_if_neovm_enable_oracle_proptest_not_set};

#[test]
fn reader_core_symbols_keep_shorthand_and_quoting_semantics() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // Empty expectations are intentionally refreshed FROM GNU before verify.
    let expected = expect_test::expect![[
        r#""OK (((((nil 3 t nil (110 105 108)) (nil 1 nil t (116)) (nil 4 t nil (110 105 108)) (nil 2 nil t (116)) (nil 4 t nil (110 105 108))) ((t 3 t nil (110 105 108)) (t 1 nil t (116)) (t 4 t nil (110 105 108)) (t 2 nil t (116)) (t 4 t nil (110 105 108)))) (((nil 3 nil nil (103 100 106 45 99 111 114 101 45 110 105 108 45)) (nil 1 nil nil (103 100 106 45 99 111 114 101 45 116 114 117 116 104 45)) (nil 4 nil nil (103 100 106 45 99 111 114 101 45 110 105 108 45)) (nil 2 nil nil (103 100 106 45 99 111 114 101 45 116 114 117 116 104 45)) (nil 4 nil nil (103 100 106 45 99 111 114 101 45 110 105 108 45))) ((t 3 nil nil (103 100 106 45 99 111 114 101 45 110 105 108 45)) (t 1 nil nil (103 100 106 45 99 111 114 101 45 116 114 117 116 104 45)) (t 4 nil nil (103 100 106 45 99 111 114 101 45 110 105 108 45)) (t 2 nil nil (103 100 106 45 99 111 114 101 45 116 114 117 116 104 45)) (t 4 nil nil (103 100 106 45 99 111 114 101 45 110 105 108 45)))) (((nil 3 t nil (110 105 108)) (nil 1 nil t (116)) (nil 4 t nil (110 105 108)) (nil 2 nil t (116)) (nil 4 t nil (110 105 108))) ((t 3 t nil (110 105 108)) (t 1 nil t (116)) (t 4 t nil (110 105 108)) (t 2 nil t (116)) (t 4 t nil (110 105 108))))) ((103 100 106 45 108 105 118 101 45 110 105 108 45) (103 100 106 45 108 105 118 101 45 116 114 117 116 104 45) (103 100 106 45 108 105 118 101 45 110 105 108 45) (103 100 106 45 108 105 118 101 45 116 114 117 116 104 45)))""#
    ]];
    assert_oracle_parity_expect(
        r###"(list
 (mapcar (lambda (binding)
   (let ((read-symbol-shorthands binding))
     (mapcar (lambda (multibyte)
       (mapcar (lambda (text)
         (let* ((source (if multibyte (string-to-multibyte text)
                         (string-to-unibyte text)))
                (pair (read-from-string source)) (value (car pair)))
           (list (multibyte-string-p source) (cdr pair)
                 (eq value nil) (eq value t)
                 (append (symbol-name value) nil))))
         '("nil" "t" "\\nil" "\\t" "n\\il")))
       '(nil t))))
   '(nil (("nil" . "gdj-core-nil-") ("t" . "gdj-core-truth-"))
         (17 ("bad" . 23) (bad . "bad"))))
 (mapcar (lambda (text)
   (let ((read-symbol-shorthands nil) (input (string-to-list (concat text " "))))
     (let ((value (read (lambda (&optional c)
       (if c (progn
               (setq read-symbol-shorthands
                     '(("nil" . "gdj-live-nil-") ("t" . "gdj-live-truth-")))
               (setq input (cons c input)))
         (let ((ch (car input))) (setq input (cdr input)) ch))))))
       (append (symbol-name value) nil))))
   '("nil" "t" "\\nil" "\\t")))"###,
        expected,
    );
}

fn core_reintern_form(spelling: &str, source_conversion: &str) -> String {
    format!(
        r###"(let* ((read-symbol-shorthands nil) (spelling "{spelling}")
        (source ({source_conversion} spelling))
        (original (intern spelling))
        (warm (car (read-from-string source))))
  (unintern original nil)
  (let* ((replacement (car (read-from-string source)))
         (warm-replacement (car (read-from-string source)))
         (interned-replacement (intern spelling)))
    (unintern replacement nil)
    (let* ((second (car (read-from-string source)))
           (warm-second (car (read-from-string source))))
      (list (eq warm original) (eq replacement original)
            (eq replacement warm-replacement)
            (eq replacement interned-replacement)
            (multibyte-string-p (symbol-name replacement))
            (eq second replacement) (eq second original)
            (eq second warm-second) (eq second (intern spelling))
            (multibyte-string-p (symbol-name second))))))"###
    )
}

#[test]
fn reader_core_symbols_revalidate_identity_after_unintern_and_reintern() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // Every helper call starts NEW GNU and Neo editor subprocesses. A core
    // removal cannot leak into another case or concurrently executing test.
    let cases = [
        (
            "nil",
            "string-to-unibyte",
            expect_test::expect![[r#""OK (t nil t t nil nil nil t t nil)""#]],
        ),
        (
            "nil",
            "string-to-multibyte",
            expect_test::expect![[r#""OK (t nil t t t nil nil t t t)""#]],
        ),
        (
            "t",
            "string-to-unibyte",
            expect_test::expect![[r#""OK (t nil t t nil nil nil t t nil)""#]],
        ),
        (
            "t",
            "string-to-multibyte",
            expect_test::expect![[r#""OK (t nil t t t nil nil t t t)""#]],
        ),
    ];
    for (spelling, conversion, expected) in cases {
        assert_oracle_parity_expect(&core_reintern_form(spelling, conversion), expected);
    }
}

#[test]
fn reader_core_symbol_lookup_keeps_binding_and_checked_obarray_semantics() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // Each form starts independent editor subprocesses. Alias state and the
    // legacy checked-obarray mutation do not escape into another test.
    let binding_expected =
        expect_test::expect![[r#""OK ((t t) nil (t (\"gdj-perf-local-binding-\" nil) t) t)""#]];
    assert_oracle_parity_expect(
        r###"(progn
  (defvaralias 'gdj-perf-shorthands-alias 'read-symbol-shorthands)
  (let ((read-symbol-shorthands nil))
    (list
      (let ((gdj-perf-shorthands-alias '(("gdj-perf-binding-" . "car"))))
        (list (eq (intern "gdj-perf-binding-") 'car)
              (eq (intern-soft "gdj-perf-binding-") 'car)))
      (intern-soft "gdj-perf-binding-")
      (with-temp-buffer
        (setq-local read-symbol-shorthands '(("gdj-perf-local-binding-" . "cdr")))
        (list (eq (intern "gdj-perf-local-binding-") 'cdr)
              (let ((read-symbol-shorthands nil))
                (list (symbol-name (intern "gdj-perf-local-binding-"))
                      (eq (intern-soft "gdj-perf-local-binding-") 'cdr)))
              (eq (intern-soft "gdj-perf-local-binding-") 'cdr)))
      (with-temp-buffer
        (let ((gdj-perf-shorthands-alias '(("gdj-perf-other-binding-" . "car"))))
          (eq (intern-soft "gdj-perf-other-binding-") 'car))))))"###,
        binding_expected,
    );
    let checked_obarray_expected = expect_test::expect![[
        r#""OK ((wrong-type-argument stringp 17) t (t nil t) (wrong-type-argument obarrayp 17))""#
    ]];
    assert_oracle_parity_expect(
        r###"(let ((ob (vector 0)) (read-symbol-shorthands nil))
  (list
    (condition-case error-value (intern-soft 17 ob) (error error-value))
    (obarrayp (aref ob 0))
    (let ((symbol (intern "gdj-perf-retained-checked-obarray" ob)))
      (list (eq (intern-soft symbol ob) symbol)
            (intern-soft (make-symbol "gdj-perf-retained-checked-obarray") ob)
            (eq (intern-soft "gdj-perf-retained-checked-obarray" ob) symbol)))
    (condition-case error-value (intern-soft 17 17) (error error-value))))"###,
        checked_obarray_expected,
    );
}
