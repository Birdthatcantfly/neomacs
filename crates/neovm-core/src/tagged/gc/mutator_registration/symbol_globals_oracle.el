;; -*- lexical-binding: t -*-
;; C3 serial twins. Refresh ONLY with GNU 31.1 through sandbox-run.sh,
;; NEOVM_ORACLE_MODE=refresh UPDATE_EXPECT=1. No executor/name-growth op.
(unless (and (string= emacs-version "31.1")
             (equal (getenv "NEOVM_ORACLE_MODE") "refresh")
             (equal (getenv "UPDATE_EXPECT") "1"))
  (error "C3 oracle requires GNU 31.1 and explicit refresh mode"))
(defun p3sym-c3-out (value) (prin1 value) (terpri))

;; First four lines are the individual primitive red tests. All names and
;; mutable storage are created by serial setup, before an owner could run.
(set 'p3sym-c3-global 73)
(p3sym-c3-out (symbol-value 'p3sym-c3-global))
(p3sym-c3-out (set 'p3sym-c3-global 97))
(p3sym-c3-out (boundp 'p3sym-c3-global))
(p3sym-c3-out (eq (makunbound 'p3sym-c3-global) 'p3sym-c3-global))

(p3sym-c3-out
 (list (boundp 'p3sym-c3-global)
       (condition-case error (symbol-value 'p3sym-c3-global)
         (void-variable (car error)))))
(p3sym-c3-out
 (list (set 'p3sym-c3-global nil) (boundp 'p3sym-c3-global)
       (symbol-value 'p3sym-c3-global)))
(makunbound 'p3sym-c3-global)
(p3sym-c3-out
 (list (set 'p3sym-c3-global 101) (symbol-value 'p3sym-c3-global)
       (eq (makunbound 'p3sym-c3-global) 'p3sym-c3-global)
       (boundp 'p3sym-c3-global)))

;; Primitive serial histories, indexed by writer identity, not completion
;; order. Concurrent tests must accept both legal final states.
(p3sym-c3-out
 (let* ((writer-a (set 'p3sym-c3-global 11))
        (writer-b (set 'p3sym-c3-global 22)))
   (list writer-a writer-b (symbol-value 'p3sym-c3-global))))
(p3sym-c3-out
 (let* ((writer-b (set 'p3sym-c3-global 22))
        (writer-a (set 'p3sym-c3-global 11)))
   (list writer-a writer-b (symbol-value 'p3sym-c3-global))))
(p3sym-c3-out
 (let ((answer (set 'p3sym-c3-global 11)))
   (set 'p3sym-c3-global 22)
   (list answer (symbol-value 'p3sym-c3-global))))

;; Read plus write is two primitives: a lost update is a legal history.
(set 'p3sym-c3-global 0)
(p3sym-c3-out
 (let ((a (symbol-value 'p3sym-c3-global))
       (b (symbol-value 'p3sym-c3-global)))
   (list (set 'p3sym-c3-global (1+ a))
         (set 'p3sym-c3-global (1+ b))
         (symbol-value 'p3sym-c3-global))))
(set 'p3sym-c3-global 0)
(p3sym-c3-out
 (list (set 'p3sym-c3-global (1+ (symbol-value 'p3sym-c3-global)))
       (set 'p3sym-c3-global (1+ (symbol-value 'p3sym-c3-global)))
       (symbol-value 'p3sym-c3-global)))

;; An answer is a root in its own right after a global is overwritten.
(set 'p3sym-c3-global (list 41 42))
(p3sym-c3-out
 (let ((answer (symbol-value 'p3sym-c3-global)))
   (makunbound 'p3sym-c3-global)
   (garbage-collect)
   (list answer (boundp 'p3sym-c3-global))))

;; GNU threads pin the existing dynamic-binding fallback semantics. C3 adds
;; canonical global operations only; it adds no bind/unbind opcode.
(defvar p3sym-c3-dynamic 7)
(p3sym-c3-out
 (let ((p3sym-c3-dynamic 99))
   (list p3sym-c3-dynamic
         (thread-join
          (make-thread
           (lambda ()
             (list (symbol-value 'p3sym-c3-dynamic)
                   (let ((p3sym-c3-dynamic 17))
                     (set 'p3sym-c3-dynamic 18))
                   (symbol-value 'p3sym-c3-dynamic)))))
         p3sym-c3-dynamic)))
(p3sym-c3-out (symbol-value 'p3sym-c3-dynamic))

;; These operations remain admission refusals in the closed executor. GNU
;; records their language behavior; no keyword-creation operation is added.
(p3sym-c3-out
 (mapcar (lambda (symbol)
           (condition-case error (set symbol 8)
             (setting-constant (list (car error) symbol))))
         '(nil t :p3sym-c3-constant)))
