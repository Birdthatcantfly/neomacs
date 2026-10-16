//! Green alias regressions mirrored from `oracle_green_threads`.
//!
//! GNU Emacs 31.1 supplied these expected values through the refreshed oracle
//! family. Every handoff uses a mutex/condition handshake and a final join.
//! Each test creates one World; nextest runs each case in its own process.

use super::{eval, green};

#[test]
fn nested_aliases_merge_with_existing_target_bindings() {
    let mut ev = green();
    // The bare Context has the primitive null but no Lisp definition of not.
    eval(&mut ev, "(fset 'not 'null)");
    assert_eq!(
        eval(
            &mut ev,
            r##"(eval '
(progn
(defvar g3e-source 'source-global)
(defvar g3e-target 'target-global)
(defvaralias 'g3e-link 'g3e-target)
(let* ((mutex (make-mutex)) (cv (make-condition-variable mutex))
       (ready nil) (go nil)
       (worker nil))
  (mutex-lock mutex)
  (setq worker
        (make-thread
         (lambda ()
           (list
            (let ((g3e-target 'target-outer))
              (list
               (let ((g3e-source 'source-outer))
                 (list
                  (let ((g3e-target 'target-inner))
                    (list
                     (let ((g3e-source 'source-inner))
                       (mutex-lock mutex)
                       (setq ready t)
                       (condition-notify cv)
                       (while (not go) (condition-wait cv))
                       (mutex-unlock mutex)
                       (list g3e-source g3e-target))
                     (list g3e-source g3e-target)))
                  (list g3e-source g3e-target)))
               (list g3e-source g3e-target)))
            (list g3e-source g3e-target)))))
  (while (not ready) (condition-wait cv))
  (defvaralias 'g3e-source 'g3e-link)
  (setq go t)
  (condition-notify cv)
  (mutex-unlock mutex)
  (list (thread-join worker) g3e-source g3e-target (indirect-variable 'g3e-source)))
)
 t)"##,
        ),
        r##"((((((source-inner source-inner) (target-inner target-inner)) (source-outer source-outer)) (target-outer target-outer)) (target-global target-global)) target-global target-global g3e-target)"##,
    );
}

#[test]
fn an_alias_to_a_void_target_materializes_its_owner_slot() {
    let mut ev = green();
    // The bare Context has the primitive null but no Lisp definition of not.
    eval(&mut ev, "(fset 'not 'null)");
    assert_eq!(
        eval(
            &mut ev,
            r##"(eval '
(progn
  (defvar g3e-source)
  (defvar g3e-target)
  (let* ((mutex (make-mutex))
         (condition (make-condition-variable mutex))
         (ready nil) (go nil) (worker nil))
    (mutex-lock mutex)
    (setq worker
          (make-thread
           (lambda ()
             (let ((g3e-source 'source-bound))
               (mutex-lock mutex)
               (setq ready t)
               (condition-notify condition)
               (while (not go) (condition-wait condition))
               (mutex-unlock mutex)
               (list g3e-source g3e-target)))))
    (while (not ready) (condition-wait condition))
    (defvaralias 'g3e-source 'g3e-target)
    (let ((before (boundp 'g3e-target)))
      (setq go t)
      (condition-notify condition)
      (mutex-unlock mutex)
      (list before (thread-join worker) (boundp 'g3e-source) (boundp 'g3e-target)))))
 t)"##,
        ),
        r##"(nil (source-bound source-bound) nil nil)"##,
    );
}

#[test]
fn an_alias_binding_follows_the_default_of_a_localized_target() {
    let mut ev = green();
    // The bare Context has the primitive null but no Lisp definition of not.
    eval(&mut ev, "(fset 'not 'null)");
    assert_eq!(
        eval(
            &mut ev,
            r##"(eval '
(progn
  (defvar g3e-source 'source-global)
  (defvar g3e-target 'target-global)
  (progn
    (set-buffer (get-buffer-create "g3e-localized-unit"))
    (set (make-local-variable 'g3e-target) 'target-local)
  (let* ((mutex (make-mutex))
         (condition (make-condition-variable mutex))
         (ready nil) (go nil) (worker nil))
    (mutex-lock mutex)
    (setq worker
          (make-thread
           (lambda ()
             (let ((g3e-source 'source-bound))
               (mutex-lock mutex)
               (setq ready t)
               (condition-notify condition)
               (while (not go) (condition-wait condition))
               (mutex-unlock mutex)
               (list g3e-source g3e-target (default-toplevel-value 'g3e-source) (default-toplevel-value 'g3e-target))))))
    (while (not ready) (condition-wait condition))
    (let ((aliased (defvaralias 'g3e-source 'g3e-target)))
      (setq go t)
      (condition-notify condition)
      (mutex-unlock mutex)
      (list aliased (thread-join worker) g3e-source g3e-target)))))
 t)"##,
        ),
        r##"(g3e-target (target-local target-local target-global source-bound) target-local target-local)"##,
    );
}

#[test]
fn an_alias_to_a_constant_signals_when_its_owner_resumes() {
    let mut ev = green();
    // The bare Context has the primitive null but no Lisp definition of not.
    eval(&mut ev, "(fset 'not 'null)");
    assert_eq!(
        eval(
            &mut ev,
            r##"(eval '
(progn
(progn
  (defvar g3e-source 'source-global)

  (let* ((mutex (make-mutex))
         (condition (make-condition-variable mutex))
         (ready nil) (go nil) (worker nil))
    (mutex-lock mutex)
    (setq worker
          (make-thread
           (lambda ()
             (let ((g3e-source 'source-bound))
               (mutex-lock mutex)
               (setq ready t)
               (condition-notify condition)
               (while (not go) (condition-wait condition))
               (mutex-unlock mutex)
               (list g3e-source t (default-toplevel-value 'g3e-source) (default-toplevel-value 't))))))
    (while (not ready) (condition-wait condition))
    (let ((aliased (defvaralias 'g3e-source 't)))
      (setq go t)
      (condition-notify condition)
      (mutex-unlock mutex)
      (list aliased (thread-join worker) (thread-last-error t) g3e-source t))))

)
 t)"##,
        ),
        r##"(t nil (setting-constant g3e-source) t t)"##,
    );
}

#[test]
fn a_constant_alias_signals_again_when_its_binding_pops() {
    let mut ev = green();
    // The bare Context has the primitive null but no Lisp definition of not.
    eval(&mut ev, "(fset 'not 'null)");
    assert_eq!(
        eval(
            &mut ev,
            r##"(eval '
(progn
(defvar g3e-source 'source-global)
(let* ((mutex (make-mutex)) (cv (make-condition-variable mutex))
       (ready nil) (go nil) (worker nil))
  (mutex-lock mutex)
  (setq worker
        (make-thread
         (lambda ()
           (let ((g3e-source 'source-bound))
             (mutex-lock mutex)
             (setq ready t)
             (condition-notify cv)
             (condition-case e
                 (progn
                   (while (not go) (condition-wait cv))
                   (list 'no-error (mutex-unlock mutex)))
               (error
                (list 'caught e
                      (condition-case e2 (mutex-unlock mutex) (error e2))
                      g3e-source)))))))
  (while (not ready) (condition-wait cv))
  (defvaralias 'g3e-source 't)
  (setq go t)
  (condition-notify cv)
  (mutex-unlock mutex)
  (list (thread-join worker) (thread-last-error t) g3e-source))
)
 t)"##,
        ),
        r##"(nil (setting-constant g3e-source) t)"##,
    );
}

#[test]
fn a_constant_alias_retakes_the_condition_mutex_before_signalling() {
    let mut ev = green();
    // The bare Context has the primitive null but no Lisp definition of not.
    eval(&mut ev, "(fset 'not 'null)");
    assert_eq!(
        eval(
            &mut ev,
            r##"(eval '
(progn
(defvar g3e-source 'source-global)
(let* ((mutex (make-mutex)) (cv (make-condition-variable mutex))
       (ready nil) (go nil) (caught nil) (worker nil))
  (mutex-lock mutex)
  (setq worker
        (make-thread
         (lambda ()
           (condition-case e
               (let ((g3e-source 'source-bound))
                 (mutex-lock mutex)
                 (setq ready t)
                 (condition-notify cv)
                 (while (not go) (condition-wait cv))
                 (mutex-unlock mutex)
                 'no-error)
             (error (setq caught e)
                    (list 'caught e (condition-case e2 (mutex-unlock mutex) (error e2))))))))
  (while (not ready) (condition-wait cv))
  (defvaralias 'g3e-source 't)
  (setq go t)
  (condition-notify cv)
  (mutex-unlock mutex)
  (list (thread-join worker) caught (thread-last-error t) g3e-source))
)
 t)"##,
        ),
        r##"((caught (setting-constant g3e-source) nil) (setting-constant g3e-source) nil t)"##,
    );
}
