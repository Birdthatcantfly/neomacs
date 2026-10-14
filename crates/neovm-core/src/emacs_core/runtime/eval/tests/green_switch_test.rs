//! Phase 0 step 2, round 3d: Lisp threads switching on the Baton.

use crate::emacs_core::eval::{Context, ThreadingMode};
use crate::emacs_core::print::print_value;

fn eval(ev: &mut Context, src: &str) -> String {
    match ev.eval_str(src) {
        Ok(value) => print_value(&value),
        Err(error) => format!("ERR {error:?}"),
    }
}

fn green() -> Context {
    crate::test_utils::init_test_tracing();
    #[cfg(feature = "jit")]
    crate::emacs_core::jit::force_jit_off_for_test(true);
    Context::new_with_threading(ThreadingMode::Green)
}

#[test]
fn a_thread_runs_and_join_returns_its_value() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(let ((th (make-thread (lambda () (+ 1 2))))) (thread-join th))"
        ),
        "3"
    );
}

#[test]
fn yields_interleave_threads_in_turn() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgs-log nil)
               (let ((a (make-thread (lambda ()
                                       (setq tgs-log (cons 'a1 tgs-log))
                                       (thread-yield)
                                       (setq tgs-log (cons 'a2 tgs-log)))))
                     (b (make-thread (lambda ()
                                       (setq tgs-log (cons 'b1 tgs-log))
                                       (thread-yield)
                                       (setq tgs-log (cons 'b2 tgs-log))))))
                 (thread-join a)
                 (thread-join b)
                 (nreverse tgs-log)))"
        ),
        "(a1 b1 a2 b2)"
    );
}

#[test]
fn a_new_thread_sees_global_values_not_its_creators_bindings() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (defvar tgs-v 0)
               (let ((tgs-v 1))
                 (list tgs-v
                       (thread-join (make-thread (lambda () tgs-v)))
                       (thread-join (make-thread (lambda () (let ((tgs-v 2)) (thread-yield) tgs-v))))
                       tgs-v)))"
        ),
        "(1 0 2 1)"
    );
    assert_eq!(eval(&mut ev, "tgs-v"), "0");
}

#[test]
fn a_parked_threads_values_survive_another_threads_collection() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(let ((kept (make-list 1000 'kept)))
               (thread-join
                (make-thread (lambda ()
                               (let ((i 0))
                                 (while (< i 2000)
                                   (make-list 50 i)
                                   (setq i (1+ i))))
                               (garbage-collect))))
               (list (length kept) (car kept) (nth 999 kept)))"
        ),
        "(1000 kept kept)"
    );
}

#[test]
fn joining_the_current_thread_signals() {
    let mut ev = green();
    eval(&mut ev, "(thread-join (make-thread (lambda () nil)))");
    assert_eq!(
        eval(
            &mut ev,
            "(condition-case e (thread-join (current-thread)) (error (car (cdr e))))"
        ),
        "\"Cannot join current thread\""
    );
}
