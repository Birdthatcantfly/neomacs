//! GNU funcall_general and condition-case retain active SYMBOLP policy.
use crate::common::{
    assert_ok_eq, eval_oracle_and_neovm, return_if_neovm_enable_oracle_proptest_not_set,
};

#[test]
fn issue554_oracle_positioned_funcall_uses_active_symbol_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(let ((target (position-symbol '+ 0)))
  (mapcar (lambda (enabled)
            (let ((symbols-with-pos-enabled enabled))
              (condition-case err (funcall target 2 3)
                (invalid-function
                 (list 'invalid-function (eq (car (cdr err)) target))))))
          '(nil t nil)))"#,
    );
    assert_ok_eq(
        "((invalid-function t) 5 (invalid-function t))",
        &oracle,
        &neo,
    );
}

#[test]
fn issue554_oracle_positioned_apply_uses_active_symbol_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(let ((target (position-symbol '+ 0)))
  (mapcar (lambda (enabled)
            (let ((symbols-with-pos-enabled enabled))
              (condition-case err (apply target '(2 3))
                (invalid-function
                 (list 'invalid-function (eq (car (cdr err)) target))))))
          '(nil t nil)))"#,
    );
    assert_ok_eq(
        "((invalid-function t) 5 (invalid-function t))",
        &oracle,
        &neo,
    );
}

#[test]
fn issue554_oracle_positioned_named_alias_uses_active_symbol_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(progn
  (fset 'issue554-positioned-function-alias (position-symbol '+ 0))
  (unwind-protect
      (mapcar (lambda (enabled)
                (let ((symbols-with-pos-enabled enabled))
                  (condition-case err
                      (funcall 'issue554-positioned-function-alias 2 3)
                    (invalid-function
                     (list 'invalid-function
                           (eq (car (cdr err)) 'issue554-positioned-function-alias))))))
              '(nil t nil))
    (fmakunbound 'issue554-positioned-function-alias)))"#,
    );
    assert_ok_eq(
        "((invalid-function t) 5 (invalid-function t))",
        &oracle,
        &neo,
    );
}

#[test]
fn issue554_oracle_positioned_evaluated_alias_uses_active_symbol_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(progn
  (fset 'issue554-positioned-eval-alias (position-symbol '+ 0))
  (unwind-protect
      (mapcar (lambda (enabled)
                (let ((symbols-with-pos-enabled enabled))
                  (condition-case err
                      (eval '(issue554-positioned-eval-alias 2 3) nil)
                    (invalid-function
                     (list 'invalid-function
                           (eq (car (cdr err)) 'issue554-positioned-eval-alias))))))
              '(nil t nil))
    (fmakunbound 'issue554-positioned-eval-alias)))"#,
    );
    assert_ok_eq(
        "((invalid-function t) 5 (invalid-function t))",
        &oracle,
        &neo,
    );
}

#[test]
fn issue554_oracle_positioned_handler_uses_active_symbol_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(progn
  (defvar issue554-positioned-handler-ran nil)
  (mapcar
   (lambda (enabled)
     (let ((symbols-with-pos-enabled enabled)
           (issue554-positioned-handler-ran nil)
           (condition (position-symbol 'error 0)))
       (list
        (condition-case err
            (eval (list 'condition-case nil
                        '(progn (setq issue554-positioned-handler-ran t)
                                (error "body"))
                        (list condition '(quote caught)))
                  nil)
          (error (car err)))
        issue554-positioned-handler-ran)))
   '(nil t nil)))"#,
    );
    assert_ok_eq("((error nil) (caught t) (error nil))", &oracle, &neo);
}

#[test]
fn issue554_oracle_compiled_funcall_uses_active_symbol_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(progn
  (require 'bytecomp)
  (let ((function (byte-compile '(lambda (target) (funcall target 2 3))))
        (target (position-symbol '+ 0)))
    (mapcar (lambda (enabled)
              (let ((symbols-with-pos-enabled enabled))
                (condition-case err (funcall function target)
                  (invalid-function
                   (list 'invalid-function (eq (car (cdr err)) target))))))
            '(nil t nil))))"#,
    );
    assert_ok_eq(
        "((invalid-function t) 5 (invalid-function t))",
        &oracle,
        &neo,
    );
}

#[test]
fn issue554_oracle_positioned_resolution_preserves_original_error_operands() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(let ((symbols-with-pos-enabled t))
  (fmakunbound 'issue554-positioned-void)
  (fset 'issue554-positioned-invalid 42)
  (unwind-protect
      (list
       (let ((target (position-symbol 'issue554-positioned-void 1)))
         (condition-case err (funcall target)
           (error (list (car err)
                        (let ((symbols-with-pos-enabled nil))
                          (eq (car (cdr err)) target))))))
       (let ((target (position-symbol 'issue554-positioned-invalid 2)))
         (condition-case err (funcall target)
           (error (list (car err)
                        (let ((symbols-with-pos-enabled nil))
                          (eq (car (cdr err)) target))))))
       (mapcar
        (lambda (condition)
          (fset 'issue554-positioned-user-signal
                (list 'lambda nil
                      '(fset 'issue554-positioned-user-signal 42)
                      (list 'signal (list 'quote condition) '(quote (payload)))))
          (condition-case err
              (funcall (position-symbol 'issue554-positioned-user-signal 3))
            (error err)))
        '(invalid-function void-function)))
    (fmakunbound 'issue554-positioned-invalid)
    (fmakunbound 'issue554-positioned-user-signal)))"#,
    );
    assert_ok_eq(
        "((void-function t) (invalid-function t) ((invalid-function payload) (void-function payload)))",
        &oracle,
        &neo,
    );
}

#[test]
fn issue554_oracle_positioned_true_function_preserves_original_error_operands() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    let (oracle, neo) = eval_oracle_and_neovm(
        r#"(let ((symbols-with-pos-enabled t)
      (original (and (fboundp t) (symbol-function t))))
  (unwind-protect
      (mapcar
       (lambda (condition)
         (fset t (list 'lambda nil
                       '(fset t 42)
                       (list 'signal (list 'quote condition) '(quote (payload)))))
         (condition-case err (funcall (position-symbol t 4))
           (error err)))
       '(invalid-function void-function))
    (fset t original)))"#,
    );
    assert_ok_eq(
        "((invalid-function payload) (void-function payload))",
        &oracle,
        &neo,
    );
}
