//! GNU SYMBOLP/EQ policy at the final public builtin boundaries.
use crate::common::{
    assert_ok_eq, eval_oracle_and_neovm, return_if_neovm_enable_oracle_proptest_not_set,
};

fn check(form: &str, expected: &str) {
    let (gnu, neomacs) = eval_oracle_and_neovm(form);
    assert_ok_eq(expected, &gnu, &neomacs);
}

#[test]
fn issue554_sort_keywords_and_fast_designators_follow_eq_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // fns.c:2486-2496 EQ(option,key/lessp/in-place/reverse).
    check(
        r#"(let ((key (position-symbol :key 0))
                   (identity (position-symbol 'identity 1))
                   (lessp (position-symbol :lessp 2))
                   (value< (position-symbol 'value< 3)))
                 (mapcar (lambda (enabled)
                   (let ((symbols-with-pos-enabled enabled))
                     (condition-case err
                       (sort [3 1 2] key identity lessp value<
                             (position-symbol :reverse 4) nil
                             (position-symbol :in-place 5) nil)
                       (error (car err))))) '(nil t)))"#,
        "(error [1 2 3])",
    );
}

#[test]
fn issue554_fringe_alignment_and_frame_symbol_checks_follow_mode() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // fringe.c:1613-1617 EQ; dispnew.c:7069 CHECK_SYMBOL.
    check(
        r#"(let ((top (position-symbol 'top 0))
                   (variable (position-symbol 'issue554-void-state 1)))
                 (mapcar (lambda (enabled)
                   (let ((symbols-with-pos-enabled enabled))
                     (list (condition-case err
                             (progn (define-fringe-bitmap 'issue554-fringe [0] 1 8 (list top t)) t)
                             (error (car err)))
                           (condition-case err (frame-or-buffer-changed-p variable)
                             (error (car err)))))) '(nil t)))"#,
        "((error wrong-type-argument) (t void-variable))",
    );
}

#[test]
fn issue554_image_library_type_follows_eq_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // image.c:12911,13006 EQ(type,builtin type).
    check(
        r#"(let ((type (position-symbol 'png 0)))
                 (list (let ((symbols-with-pos-enabled nil)) (init-image-library type))
                       (let ((symbols-with-pos-enabled t))
                         (eq (init-image-library type) (init-image-library 'png)))))"#,
        "(nil t)",
    );
}

#[test]
fn issue554_completion_flags_follow_eq_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // minibuf.c:2159-2185 EQ(flag,t/lambda/metadata).
    check(
        r#"(let ((buffer (get-buffer-create "issue554-complete-target")))
                 (unwind-protect
                   (let ((symbols-with-pos-enabled t))
                     (list (internal-complete-buffer "issue554-complete-target" nil
                                                     (position-symbol 'lambda 0))
                           (eq (car (internal-complete-buffer "" nil
                                                             (position-symbol 'metadata 1))) 'metadata)
                           (equal (internal-complete-buffer "issue554-complete-target" nil
                                                            (position-symbol t 2))
                                  '("issue554-complete-target"))))
                   (kill-buffer buffer)))"#,
        "(t t t)",
    );
}

#[test]
#[cfg(target_os = "linux")]
fn issue554_inotify_aspect_symbols_follow_eq_policy() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    // inotify.c:117-159 EQ on each aspect,168-180 list/single dispatch.
    check(
        r#"(let ((file (make-temp-file "issue554-inotify-"))
                   (aspect (position-symbol 'modify 0)))
                 (unwind-protect
                   (mapcar (lambda (enabled)
                     (let ((symbols-with-pos-enabled enabled))
                       (condition-case err
                         (let ((watch (inotify-add-watch file (list aspect) #'ignore)))
                           (inotify-rm-watch watch) t)
                         (error (car err))))) '(nil t))
                   (delete-file file)))"#,
        "(file-notify-error t)",
    );
}
