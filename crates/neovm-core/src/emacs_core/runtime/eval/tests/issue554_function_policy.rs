//! GNU SYMBOLP gates function designators and handler heads before evaluation.
use crate::emacs_core::error::{FlowRef, FlowResultExt};
use crate::emacs_core::eval::{Context, TierIMode};
use crate::emacs_core::print::print_value;
use crate::emacs_core::value::Value;

fn check(source: &str, expected: &str) {
    for mode in [TierIMode::Off, TierIMode::On, TierIMode::Verify] {
        let mut context = Context::new();
        context.tier_i.set_mode(mode);
        context.tier_i.set_threshold(1);
        let result = context
            .eval_str(source)
            .expect("positioned function policy");
        assert_eq!(print_value(&result), expected, "tier {mode:?}");
    }
}

#[test]
fn issue554_general_funcall_cold_arm_checks_live_policy() {
    let mut context = Context::new();
    let function = context
        .tagged_heap
        .alloc_symbol_with_pos(Value::symbol("+"), Value::fixnum(0));
    for enabled in [false, true, false] {
        context.set_variable("symbols-with-pos-enabled", Value::bool_val(enabled));
        let result =
            context.funcall_general_untraced(function, vec![Value::fixnum(2), Value::fixnum(3)]);
        if enabled {
            assert_eq!(result.expect("enabled callable"), Value::fixnum(5));
        } else {
            let Err(FlowRef::Signal(signal)) = result.kinded_ref() else {
                panic!("disabled positioned function must signal");
            };
            assert_eq!(signal.symbol_name(), "invalid-function");
            assert_eq!(signal.data[0].bits(), function.bits());
        }
    }
}

#[test]
fn issue554_positioned_funcall_uses_active_symbol_policy() {
    check(
        r#"(let ((target (position-symbol '+ 0)))
  (mapcar (lambda (enabled)
            (let ((symbols-with-pos-enabled enabled))
              (condition-case err (funcall target 2 3)
                (invalid-function
                 (list 'invalid-function (eq (car (cdr err)) target))))))
          '(nil t nil)))"#,
        "((invalid-function t) 5 (invalid-function t))",
    );
}

#[test]
fn issue554_positioned_apply_uses_active_symbol_policy() {
    check(
        r#"(let ((target (position-symbol '+ 0)))
  (mapcar (lambda (enabled)
            (let ((symbols-with-pos-enabled enabled))
              (condition-case err (apply target '(2 3))
                (invalid-function
                 (list 'invalid-function (eq (car (cdr err)) target))))))
          '(nil t nil)))"#,
        "((invalid-function t) 5 (invalid-function t))",
    );
}

#[test]
fn issue554_positioned_named_alias_uses_active_symbol_policy() {
    check(
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
        "((invalid-function t) 5 (invalid-function t))",
    );
}

#[test]
fn issue554_positioned_evaluated_alias_uses_active_symbol_policy() {
    check(
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
        "((invalid-function t) 5 (invalid-function t))",
    );
}

#[test]
fn issue554_positioned_handler_uses_active_symbol_policy() {
    check(
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
        "((error nil) (caught t) (error nil))",
    );
}
