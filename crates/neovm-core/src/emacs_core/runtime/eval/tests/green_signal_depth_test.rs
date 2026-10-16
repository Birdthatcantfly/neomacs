//! Signal-time depth reserves follow the admitted owner's binding channel.

use crate::emacs_core::eval::{Context, ThreadingMode};
use crate::emacs_core::value::Value;

#[test]
fn green_signal_hook_reads_projected_integer_depth_after_admission() {
    let mut context = Context::new_with_threading(ThreadingMode::Green);
    let result = context.eval_str(
        "(progn
           (setq green-depth-hook-ran nil)
           (thread-join (make-thread (lambda () nil)))
           (setq signal-hook-function
                 (lambda (_kind _data) (setq green-depth-hook-ran t)))
           (condition-case nil (car 7) (error nil))
           (setq signal-hook-function nil)
           green-depth-hook-ran)",
    );
    context.green_teardown();
    assert_eq!(result.unwrap(), Value::T);
    assert!(context.specpdl.is_empty());
}

fn reserve_binding(localized: bool) {
    let mut context = Context::new_with_threading(ThreadingMode::Green);
    context.set_lexical_binding(true);
    context
        .eval_str(
            "(progn
           (setq green-depth-writes nil)
           (defalias 'green-depth-watcher
             (lambda (_symbol _value operation _where)
               (setq green-depth-writes (cons operation green-depth-writes))))
           (setq debugger
             (lambda (&rest _args)
               (setq green-depth-seen
                 (list max-lisp-eval-depth lisp-eval-depth-reserve))
               (thread-join (make-thread (lambda ()
                 (list max-lisp-eval-depth lisp-eval-depth-reserve)))))))",
        )
        .unwrap();
    if localized {
        context
            .eval_str(
                "(progn
               (set (make-local-variable 'max-lisp-eval-depth) 400)
               (set (make-local-variable 'lisp-eval-depth-reserve) 80))",
            )
            .unwrap();
    } else {
        context.set_variable("max-lisp-eval-depth", Value::fixnum(400));
        context.set_variable("lisp-eval-depth-reserve", Value::fixnum(80));
    }
    context
        .eval_str("(thread-join (make-thread (lambda () nil)))")
        .unwrap();
    context
        .eval_str(
            "(progn
           (add-variable-watcher 'max-lisp-eval-depth #'green-depth-watcher)
           (add-variable-watcher 'lisp-eval-depth-reserve #'green-depth-watcher))",
        )
        .unwrap();
    let saved = context.specpdl.len();
    context
        .specbind_uncached(
            crate::emacs_core::intern::intern("max-lisp-eval-depth"),
            Value::fixnum(100),
        )
        .unwrap();
    context
        .specbind_uncached(
            crate::emacs_core::intern::intern("lisp-eval-depth-reserve"),
            Value::fixnum(200),
        )
        .unwrap();
    context.set_variable("green-depth-writes", Value::NIL);
    context.depth = 100;
    let result = context.call_debugger(vec![Value::symbol("error"), Value::NIL]);
    context.depth = 0;
    let seen = context.variable_value_by_name("green-depth-seen").unwrap();
    let restored_limit = context
        .variable_value_in_current_buffer(crate::emacs_core::intern::intern("max-lisp-eval-depth"))
        .unwrap();
    let restored_reserve = context
        .variable_value_in_current_buffer(crate::emacs_core::intern::intern(
            "lisp-eval-depth-reserve",
        ))
        .unwrap();
    let writes = context
        .variable_value_by_name("green-depth-writes")
        .unwrap();
    let outcome = result.unwrap();
    assert_eq!(crate::emacs_core::print::print_value(&seen), "(300 0)");
    // A peer has no binding on the borrowing owner's channel.
    assert_eq!(crate::emacs_core::print::print_value(&outcome), "(400 80)");
    assert_eq!(restored_limit, Value::fixnum(100));
    assert_eq!(restored_reserve, Value::fixnum(200));
    assert_eq!(
        writes,
        Value::NIL,
        "temporary native reserve writes bypass watchers"
    );
    context.unbind_to_result(saved).unwrap();
    context.green_teardown();
    assert_eq!(context.max_depth, 400);
    assert!(context.specpdl.is_empty());
}

#[test]
fn green_signal_reserve_borrows_owner_default_binding_without_watchers() {
    reserve_binding(false);
}

#[test]
fn green_signal_reserve_borrows_owner_local_binding_without_watchers() {
    reserve_binding(true);
}
