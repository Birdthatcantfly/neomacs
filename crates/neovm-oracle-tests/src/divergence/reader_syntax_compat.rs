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

#[test]
fn reader_syntax_rea12() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let expect = expect_test::expect![[
        r##""OK ((invalid-read-syntax \"#2305843009213693952=\") (invalid-read-syntax \"#2305843009213693952#\") (invalid-read-syntax \"#9999999999999999999\") ((a) . 24) (invalid-read-syntax \"#0001 \"))""##
    ]];
    assert_oracle_parity_expect(
        r###"(mapcar (lambda (s) (condition-case e (read-from-string s) (error e))) '("#2305843009213693952=(a)" "#2305843009213693952#" "#999999999999999999999=a" "#2305843009213693951=(a)" "#0001 =(a)"))"###,
        expect,
    );
}

#[test]
fn reader_syntax_rea13() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let expect = expect_test::expect![[
        r#""OK ((1 . 3) (1 . 3) (1 . 3) (1 . 5) (invalid-read-syntax \"integer, radix 16\") (invalid-read-syntax \"integer, radix 16\") (-1 . 4) (2 . 7))""#
    ]];
    assert_oracle_parity_expect(
        r###"(mapcar (lambda (s) (condition-case e (read-from-string s) (error e))) '("#x1_0" "#b1_0" "#o1_7" "#24r1_0" "#x_1" "#x1g" "#x-1_0" "#b00010_1"))"###,
        expect,
    );
}

#[test]
fn reader_syntax_rea32() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let expect = expect_test::expect![[
        r#""OK ((invalid-read-syntax \".\") (invalid-read-syntax \".\") (invalid-read-syntax \".\") (invalid-read-syntax \".\") ((a . 120) . 7) ((a . 1) . 8) (invalid-read-syntax \".\") (\\. . 1) ([a .b] . 6))""#
    ]];
    assert_oracle_parity_expect(
        r###"(mapcar (lambda (s) (condition-case e (read-from-string s) (error e))) '(".?" ".#x" "[a .?x]" "[a .#x]" "(a .?x)" "(a .#x1)" "." ".)" "[a .b]"))"###,
        expect,
    );
}

#[test]
fn reader_syntax_rea33() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let expect = expect_test::expect![[
        r#""OK ((error \"Invalid size in sub-char-table\") (error \"Invalid depth in sub-char-table\") (error \"Invalid size of sub-char-table\") (error \"Invalid size in sub-char-table\"))""#
    ]];
    assert_oracle_parity_expect(
        r###"(mapcar (lambda (s) (condition-case e (read-from-string s) (error e))) '("#^^[1 0]" "#^^[0 0]" "#^^[1]" "#^^[3 -1]"))"###,
        expect,
    );
}

#[test]
fn reader_syntax_rea34() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let expect = expect_test::expect![[
        r##""OK ((invalid-read-syntax \"#24x\") (invalid-read-syntax \"#1 \") (invalid-read-syntax \"#^x\") (invalid-read-syntax \"#^^x\") (invalid-read-syntax \"]\") (invalid-read-syntax \"expected )\") (invalid-read-syntax \"Invalid string property list\") (wrong-type-argument listp 2))""##
    ]];
    assert_oracle_parity_expect(
        r###"(mapcar (lambda (s) (condition-case e (read-from-string s) (error e))) '("#24x" "#1 =(a)" "#^x" "#^^x" "(a]" "(a . b c)" "#(\"abc\" 0 1)" "#s(hash-table data (a 1 . 2))"))"###,
        expect,
    );
}
