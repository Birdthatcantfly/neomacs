;; -*- lexical-binding: t -*-
;; C4 serial language twins; expectations come ONLY from GNU 31.1 refreshes.
(unless (and (string= emacs-version "31.1")
             (equal (getenv "NEOVM_ORACLE_MODE") "refresh")
             (equal (getenv "UPDATE_EXPECT") "1"))
  (error "C4 oracle requires GNU 31.1 and explicit refresh mode"))
(defun p3sym-c4-out (value) (prin1 value) (terpri))
(setq p3sym-c4-old '(lambda () 73)
      p3sym-c4-new '(lambda () 97))
(fset 'p3sym-c4-cell p3sym-c4-old)
;; First four rows are the primitive witnesses.
(p3sym-c4-out (eq (symbol-function 'p3sym-c4-cell) p3sym-c4-old))
(p3sym-c4-out (eq (fset 'p3sym-c4-cell p3sym-c4-new) p3sym-c4-new))
(p3sym-c4-out (eq (fmakunbound 'p3sym-c4-cell) 'p3sym-c4-cell))
(fset 'p3sym-c4-cell p3sym-c4-old)
(p3sym-c4-out (funcall 'p3sym-c4-cell))

(fset 'p3sym-c4-cell p3sym-c4-new)
(p3sym-c4-out (list (funcall (symbol-function 'p3sym-c4-cell))
                    (funcall 'p3sym-c4-cell)))
(fset 'p3sym-c4-cell p3sym-c4-old)
(p3sym-c4-out
 (let ((selected (symbol-function 'p3sym-c4-cell)))
   (fset 'p3sym-c4-cell p3sym-c4-new)
   (list (funcall selected) (funcall 'p3sym-c4-cell))))
(fset 'p3sym-c4-cell p3sym-c4-old)
(p3sym-c4-out
 (let ((selected (symbol-function 'p3sym-c4-cell)))
   (fmakunbound 'p3sym-c4-cell)
   (garbage-collect)
   (list (funcall selected) (symbol-function 'p3sym-c4-cell)
         (fboundp 'p3sym-c4-cell)
         (condition-case condition (funcall 'p3sym-c4-cell)
           (void-function (car condition))))))
(fset 'p3sym-c4-cell p3sym-c4-old)
(p3sym-c4-out
 (list (eq (fset 'p3sym-c4-cell p3sym-c4-old) p3sym-c4-old)
       (fboundp 'p3sym-c4-cell) (funcall 'p3sym-c4-cell)))
(p3sym-c4-out
 (list (eq (fmakunbound 'p3sym-c4-cell) 'p3sym-c4-cell)
       (eq (fmakunbound 'p3sym-c4-cell) 'p3sym-c4-cell)
       (symbol-function 'p3sym-c4-cell) (fboundp 'p3sym-c4-cell)))
(p3sym-c4-out
 (list (fset 'p3sym-c4-cell nil) (symbol-function 'p3sym-c4-cell)
       (fboundp 'p3sym-c4-cell)
       (condition-case condition (funcall 'p3sym-c4-cell)
         (void-function (car condition)))))

(fset 'p3sym-c4-left p3sym-c4-old)
(fset 'p3sym-c4-right p3sym-c4-old)
(p3sym-c4-out
 (progn (fset 'p3sym-c4-left p3sym-c4-new)
        (fset 'p3sym-c4-right '(lambda () 101))
        (list (funcall 'p3sym-c4-left) (funcall 'p3sym-c4-right))))
(p3sym-c4-out
 (progn (fset 'p3sym-c4-right p3sym-c4-old)
        (fset 'p3sym-c4-left p3sym-c4-new)
        (list (funcall 'p3sym-c4-left) (funcall 'p3sym-c4-right))))
(fset 'p3sym-c4-cell '(lambda () '(41 42)))
(p3sym-c4-out
 (let ((selected (symbol-function 'p3sym-c4-cell)))
   (fset 'p3sym-c4-cell p3sym-c4-new)
   (garbage-collect)
   (list (funcall selected) (funcall 'p3sym-c4-cell))))
(fmakunbound 'p3sym-c4-cell)
(p3sym-c4-out
 (let ((answer 23))
   (list (condition-case condition (setq answer (funcall 'p3sym-c4-cell))
           (void-function (car condition)))
         answer)))
(p3sym-c4-out
 (let ((assigned (fset 'p3sym-c4-cell p3sym-c4-new)))
   (fset 'p3sym-c4-cell p3sym-c4-old)
   (list (funcall assigned) (funcall 'p3sym-c4-cell))))
(p3sym-c4-out (list (functionp p3sym-c4-new) (interactive-form p3sym-c4-new)))
(p3sym-c4-out
 (let ((empty (make-symbol "p3sym-c4-empty")))
   (list (symbol-function empty) (fboundp empty))))
(p3sym-c4-out
 (let ((empty (make-symbol "p3sym-c4-empty")))
   (list (eq (fmakunbound empty) empty)
         (symbol-function empty) (fboundp empty))))
(p3sym-c4-out
 (let ((empty (make-symbol "p3sym-c4-empty")))
   (fmakunbound empty)
   (list (fset empty nil) (symbol-function empty) (fboundp empty))))
(fset 'p3sym-c4-cell p3sym-c4-old)
(p3sym-c4-out
 (let ((selected (symbol-function 'p3sym-c4-cell)))
   (fmakunbound 'p3sym-c4-cell)
   (fset 'p3sym-c4-cell p3sym-c4-new)
   (list (funcall selected) (funcall 'p3sym-c4-cell)
         (eq (symbol-function 'p3sym-c4-cell) p3sym-c4-new))))
