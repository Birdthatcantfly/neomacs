//! Positioned-symbol equality and hashing at the byte-compiler's mode boundary.
//!
//! Refresh empty expectations from GNU 31.1 with
//! NEOVM_ORACLE_MODE=refresh UPDATE_EXPECT=1 inside the campaign PID sandbox.
//! Hash values themselves are process identities, so observe matching hashes
//! only when positioned-symbol normalization is enabled. Disabled-mode checks
//! record GNU equality semantics and same-object hash stability instead.

use crate::common::return_if_neovm_enable_oracle_proptest_not_set;

#[test]
fn oracle_positioned_symbol_leaf_hash_mode_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let form = r#"
(let* ((bare (read "neovm--swp-hash-leaf"))
       (a (read-positioning-symbols "neovm--swp-hash-leaf"))
       (copy (read-positioning-symbols "neovm--swp-hash-leaf"))
       (moved (position-symbol bare 17))
       (other (position-symbol 'neovm--swp-hash-other 17)))
  (list
   (mapcar #'symbol-with-pos-p (list a copy moved))
   (mapcar #'symbol-with-pos-pos (list a copy moved))
   (mapcar
    (lambda (enabled)
      (let ((symbols-with-pos-enabled enabled))
        (list
         enabled
         (mapcar
          (lambda (predicate)
            (mapcar (lambda (b) (funcall predicate a b))
                    (list bare copy moved other)))
          '(eq eql equal equal-including-properties))
         (mapcar
          (lambda (hash)
            (list (integerp (funcall hash a))
                  (= (funcall hash a) (funcall hash a))))
          '(sxhash-eq sxhash-eql sxhash-equal sxhash-equal-including-properties))
         (when enabled
           (mapcar
            (lambda (hash)
              (mapcar (lambda (b) (= (funcall hash a) (funcall hash b)))
                      (list bare copy moved)))
            '(sxhash-eq sxhash-eql sxhash-equal sxhash-equal-including-properties))))))
    '(nil t nil t))))
"#;
    let expect = expect_test::expect![[
        r#""OK ((t t t) (0 0 17) ((nil ((nil nil nil nil) (nil nil nil nil) (nil t nil nil) (nil t nil nil)) ((t t) (t t) (t t) (t t)) nil) (t ((t t t nil) (t t t nil) (t t t nil) (t t t nil)) ((t t) (t t) (t t) (t t)) ((t t t) (t t t) (t t t) (t t t))) (nil ((nil nil nil nil) (nil nil nil nil) (nil t nil nil) (nil t nil nil)) ((t t) (t t) (t t) (t t)) nil) (t ((t t t nil) (t t t nil) (t t t nil) (t t t nil)) ((t t) (t t) (t t) (t t)) ((t t t) (t t t) (t t t) (t t t)))))""#
    ]];
    crate::common::assert_oracle_parity_expect(form, expect);
}

#[test]
fn oracle_positioned_symbol_nested_hash_mode_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let form = r#"
(let* ((plain-list (read "(neovm--swp-hash-a (neovm--swp-hash-b . neovm--swp-hash-c))"))
       (list-a (read-positioning-symbols "(neovm--swp-hash-a (neovm--swp-hash-b . neovm--swp-hash-c))"))
       (list-copy (read-positioning-symbols "(neovm--swp-hash-a (neovm--swp-hash-b . neovm--swp-hash-c))"))
       (list-moved (read-positioning-symbols " ( neovm--swp-hash-a ( neovm--swp-hash-b . neovm--swp-hash-c))"))
       (plain-vector (vector plain-list (car plain-list)))
       (vector-a (vector list-a (car list-a)))
       (vector-copy (vector list-copy (car list-copy)))
       (vector-moved (vector list-moved (car list-moved))))
  (list
   (symbol-with-pos-p (car list-a))
   (not (= (symbol-with-pos-pos (car list-a))
           (symbol-with-pos-pos (car list-moved))))
   (mapcar
    (lambda (enabled)
      (let ((symbols-with-pos-enabled enabled))
        (mapcar
         (lambda (objects)
           (let ((a (car objects)) (others (cdr objects)))
             (list
              (mapcar (lambda (b) (eq a b)) others)
              (mapcar (lambda (b) (eql a b)) others)
              (mapcar (lambda (b) (equal a b)) others)
              (mapcar (lambda (b) (equal-including-properties a b)) others)
              (mapcar
               (lambda (hash)
                 (list (integerp (funcall hash a))
                       (= (funcall hash a) (funcall hash a))))
               '(sxhash-equal sxhash-equal-including-properties))
              (when enabled
                (mapcar
                 (lambda (hash)
                   (mapcar (lambda (b) (= (funcall hash a) (funcall hash b))) others))
                 '(sxhash-equal sxhash-equal-including-properties))))))
         (list (list list-a plain-list list-copy list-moved)
               (list vector-a plain-vector vector-copy vector-moved)))))
    '(nil t nil t))))
"#;
    let expect = expect_test::expect![[
        r#""OK (t t ((((nil nil nil) (nil nil nil) (nil t nil) (nil t nil) ((t t) (t t)) nil) ((nil nil nil) (nil nil nil) (nil t nil) (nil t nil) ((t t) (t t)) nil)) (((nil nil nil) (nil nil nil) (t t t) (t t t) ((t t) (t t)) ((t t t) (t t t))) ((nil nil nil) (nil nil nil) (t t t) (t t t) ((t t) (t t)) ((t t t) (t t t)))) (((nil nil nil) (nil nil nil) (nil t nil) (nil t nil) ((t t) (t t)) nil) ((nil nil nil) (nil nil nil) (nil t nil) (nil t nil) ((t t) (t t)) nil)) (((nil nil nil) (nil nil nil) (t t t) (t t t) ((t t) (t t)) ((t t t) (t t t))) ((nil nil nil) (nil nil nil) (t t t) (t t t) ((t t) (t t)) ((t t t) (t t t))))))""#
    ]];
    crate::common::assert_oracle_parity_expect(form, expect);
}

#[test]
fn oracle_positioned_symbol_bytecompiled_hash_mode_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let form = r#"
(progn
  (require 'bytecomp)
  (let* ((probe
          (byte-compile
           '(lambda (a b)
              (let ((table (make-hash-table :test 'equal)))
                (puthash b 'found table)
                (list (eq a b) (eql a b)
                      (equal a b) (equal-including-properties a b)
                      (gethash a table 'missing)
                      (when symbols-with-pos-enabled
                        (list (= (sxhash-eq a) (sxhash-eq b))
                              (= (sxhash-eql a) (sxhash-eql b))
                              (= (sxhash-equal a) (sxhash-equal b))
                              (= (sxhash-equal-including-properties a)
                                 (sxhash-equal-including-properties b)))))))))
         (bare 'neovm--swp-hash-compiled)
         (a (position-symbol bare 3))
         (b (position-symbol bare 29)))
    (list
     (compiled-function-p probe)
     (mapcar
      (lambda (enabled)
        (let ((symbols-with-pos-enabled enabled))
          (list (funcall probe a bare)
                (funcall probe a b)
                (funcall probe bare a))))
      '(nil t nil t)))))
"#;
    let expect = expect_test::expect![[
        r#""OK (t (((nil nil nil nil missing nil) (nil nil nil nil missing nil) (nil nil nil nil missing nil)) ((t t t t found (t t t t)) (t t t t found (t t t t)) (t t t t found (t t t t))) ((nil nil nil nil missing nil) (nil nil nil nil missing nil) (nil nil nil nil missing nil)) ((t t t t found (t t t t)) (t t t t found (t t t t)) (t t t t found (t t t t)))))""#
    ]];
    crate::common::assert_oracle_parity_expect(form, expect);
}
