//! GNU Felt validates SEQUENCE then delegates array access to Faref.
use crate::common::{
    assert_ok_eq, eval_oracle_and_neovm, return_if_neovm_enable_oracle_proptest_not_set,
};

fn compressed_literal() -> String {
    let mut depth2 = vec!["nil".to_owned(); 32];
    depth2[2] = r#""\002\002\300\203\001""#.to_owned();
    let mut depth1 = vec!["nil".to_owned(); 16];
    depth1[0] = format!("#^^[2 0 {}]", depth2.join(" "));
    let mut contents = vec!["nil".to_owned(); 64];
    contents[0] = format!("#^^[1 0 {}]", depth1.join(" "));
    format!(
        "#^[nil nil char-code-property-table nil {} nil nil nil nil nil]",
        contents.join(" ")
    )
}

fn policy_form(compiled: bool) -> String {
    let function = if compiled {
        "(progn (require 'bytecomp) (byte-compile '(lambda (table) (elt table 256))))"
    } else {
        "(lambda (table) (elt table 256))"
    };
    format!(
        r#"(let ((function {function}) (source {source:?}))
        (mapcar
          (lambda (enabled)
            (let* ((symbols-with-pos-enabled t)
                   (table (read-positioning-symbols source)))
              (let ((symbols-with-pos-enabled enabled))
                (let ((value (funcall function table)))
                  (if (stringp value) 'compressed value)))))
          '(nil t)))"#,
        source = compressed_literal()
    )
}

#[test]
fn issue554_oracle_elt_positioned_compressed_purpose_follows_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neomacs) = eval_oracle_and_neovm(&policy_form(false));
    assert_ok_eq("(compressed 2)", &oracle, &neomacs);
}

#[test]
fn issue554_oracle_compiled_elt_positioned_compressed_purpose_follows_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neomacs) = eval_oracle_and_neovm(&policy_form(true));
    assert_ok_eq("(compressed 2)", &oracle, &neomacs);
}

#[test]
fn issue554_oracle_elt_sequence_check_precedes_index_check() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neomacs) = eval_oracle_and_neovm(
        r#"
      (list (condition-case err (elt 42 'bad) (error err))
            (condition-case err (elt [] 'bad) (error err))
            (condition-case err (elt (make-char-table 'test) -1) (error err)))
    "#,
    );
    assert_ok_eq(
        "((wrong-type-argument sequencep 42) (wrong-type-argument fixnump bad) (wrong-type-argument characterp -1))",
        &oracle,
        &neomacs,
    );
}
