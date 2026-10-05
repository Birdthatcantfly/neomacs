use crate::common::{assert_oracle_parity_expect, return_if_neovm_enable_oracle_proptest_not_set};

#[test]
fn reader_syntax_rea09() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let expect = expect_test::expect![[
        r#""OK (((97 98 99) 28 ((c d) (c d) nil)) ((97 98 99) 24 (nil (face bold) nil)) ((97 98 99) 17 ((face nil) nil nil)) (error \"Odd length text property list\") (wrong-type-argument integer-or-marker-p a) (invalid-read-syntax \".\") ((97 98 99) 26 ((a b) nil nil)) ((97 98 99) 18 ((a b) nil nil)) ((233 20013 120) 28 ((a b) (c d) (c d))))""#
    ]];
    assert_oracle_parity_expect(
        r###"(mapcar (lambda (s) (condition-case e (let* ((v (read-from-string s)) (str (car v))) (list (append str nil) (cdr v) (mapcar (lambda (n) (text-properties-at n str)) '(0 1 2)))) (error e))) '("#(\"abc\" 0 1 (a b) 0 2 (c d))" "#(\"abc\" 2 1 (face bold))" "#(\"abc\" 0 1 face)" "#(\"abc\" 0 1 (a))" "#(\"abc\" a 1 (a b))" "#(\"a\" . nil)" "#(\"abc\" 0 2 (a b) 1 3 nil)" "#(\"abc\" 0 1 (a b))" "#(\"é中x\" 0 2 (a b) 1 3 (c d))"))"###,
        expect,
    );
}
