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

#[test]
fn issue554_positioned_resolution_preserves_original_error_operands() {
    check(
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
        "((void-function t) (invalid-function t) ((invalid-function payload) (void-function payload)))",
    );
}

#[test]
fn issue554_positioned_true_function_preserves_original_error_operands() {
    check(
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
        "((invalid-function payload) (void-function payload))",
    );
}

#[test]
fn issue554_positioned_untraced_function_survives_gc_and_releases_its_root() {
    use crate::emacs_core::error::EvalResult;
    use crate::emacs_core::subr::SubrSpec;

    fn collect(context: &mut Context) -> EvalResult {
        context.gc_collect_exact();
        context
            .eval_str("(hash-table-count issue554-positioned-function-weak)")
            .map_err(crate::emacs_core::error::flow_from_eval_error)
    }

    let mut context = Context::new();
    context.register_subr(SubrSpec::fixed0("issue554-positioned-gc-callee", collect));
    context.set_variable("symbols-with-pos-enabled", Value::NIL);
    let weak = context
        .eval_str(
            "(setq issue554-positioned-function-weak
               (make-hash-table :test 'eq :weakness 'key))",
        )
        .expect("weak-key witness");
    let function = context.tagged_heap.alloc_symbol_with_pos(
        Value::symbol("issue554-positioned-gc-callee"),
        Value::fixnum(17),
    );
    context
        .apply(Value::symbol("puthash"), vec![function, Value::T, weak])
        .expect("weak witness insertion");
    context.set_variable("symbols-with-pos-enabled", Value::T);
    let specpdl_depth = context.specpdl.len();
    // This entry intentionally has no outer backtrace, argument, lexical or
    // obarray root for the wrapper: the weak hash key cannot keep it alive.
    let count = context
        .funcall_general_untraced(function, Vec::<Value>::new())
        .expect("untraced collecting positioned call");
    assert_eq!(count, Value::fixnum(1));
    assert_eq!(context.specpdl.len(), specpdl_depth);
    context.gc_collect_exact();
    // No dereference of FUNCTION after its owning scope ends. Its weak key
    // disappears, proving the temporary root was also released.
    assert_eq!(
        context
            .eval_str("(hash-table-count issue554-positioned-function-weak)")
            .expect("released weak witness"),
        Value::fixnum(0)
    );
}
