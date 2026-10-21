;;; compile-deps.el --- compile-time loads of Lisp sources  -*- lexical-binding: t; -*-

;; Run by `cargo xtask fresh-build' with the final dumped Neomacs before the
;; compile-main pass.  For every source it lists the Lisp files the byte
;; compiler loads while compiling that source, so compile-main can compile a
;; file only after the bytecode of everything it loads has been published.
;; Without that order a compiler loads its dependencies from source, and a
;; source-loaded dependency changes the compiled output (eager macroexpansion
;; loads extra libraries, `cl-deftype' predicates stay interpreted closures,
;; inline functions arrive from other files).
;;
;; The scan is static and reads each file with the Lisp reader:
;;   - `require' at top level, also inside `progn', `with-no-warnings' and
;;     `with-suppressed-warnings' (the compiler evaluates those requires);
;;   - every `require' anywhere inside `eval-when-compile', `eval-and-compile'
;;     and `cl-eval-when' bodies (the compiler evaluates the whole body);
;;   - every symbol whose definition in this image is an autoload the
;;     compiler would trigger: an autoloaded macro, or an autoloaded function
;;     with a `compiler-macro' or `byte-optimizer' property.
;; It may over-approximate (a symbol in quoted data counts); an extra edge only
;; delays a file, it never makes the order depend on timing.
;;
;; Output: one line per edge, "SOURCE<TAB>DEPENDENCY", both absolute .el
;; paths.  Dependencies outside the caller's source set are filtered by the
;; caller.

(defun xtask-compile-deps--locate (name)
  "Return the absolute source file that `load' of NAME would read, or nil."
  (let ((file (locate-file name load-path '(".el" ".el.gz"))))
    (and file (file-truename file))))

(defun xtask-compile-deps--quoted-symbol (form)
  (and (eq (car-safe form) 'quote)
       (symbolp (car-safe (cdr-safe form)))
       (cadr form)))

(defun xtask-compile-deps--require (form push)
  "Record the file loaded by the `require' call FORM through PUSH."
  (let ((feature (xtask-compile-deps--quoted-symbol (nth 1 form)))
        (filename (nth 2 form)))
    (when feature
      (funcall push (if (stringp filename) filename (symbol-name feature))))))

(defun xtask-compile-deps--walk-deep (form push)
  "Record every `require' call anywhere in FORM, a compile-time body."
  (while (consp form)
    (let ((head (car form)))
      (cond
       ((eq head 'quote) (setq form nil))
       ((eq head 'require)
        (xtask-compile-deps--require form push)
        (setq form (cdr form)))
       (t
        (when (consp head)
          (xtask-compile-deps--walk-deep head push))
        (setq form (cdr form)))))))

(defun xtask-compile-deps--walk-toplevel (form push)
  "Record the compile-time loads of the top-level FORM."
  (when (consp form)
    (pcase (car form)
      ('require (xtask-compile-deps--require form push))
      ((or 'progn 'with-no-warnings)
       (dolist (sub (cdr-safe form))
         (xtask-compile-deps--walk-toplevel sub push)))
      ('with-suppressed-warnings
       (dolist (sub (cdr-safe (cdr-safe form)))
         (xtask-compile-deps--walk-toplevel sub push)))
      ((or 'eval-when-compile 'eval-and-compile 'cl-eval-when)
       (xtask-compile-deps--walk-deep (cdr-safe form) push)))))

(defun xtask-compile-deps--autoload-file (symbol)
  "Return the file an autoload of SYMBOL loads at compile time, or nil."
  (let ((def (and (fboundp symbol) (symbol-function symbol))))
    (when (and (autoloadp def)
               (or (memq (nth 4 def) '(macro t))
                   (get symbol 'compiler-macro)
                   (get symbol 'byte-optimizer)))
      (nth 1 def))))

(defun xtask-compile-deps--walk-symbols (form push seen)
  "Record the autoload files of the symbols in FORM.
SEEN is a hash table of the conses and symbols already visited."
  (let ((stack (list form)))
    (while stack
      (let ((x (pop stack)))
        (cond
         ((symbolp x)
          (unless (gethash x seen)
            (puthash x t seen)
            (let ((file (xtask-compile-deps--autoload-file x)))
              (when file (funcall push file)))))
         ((consp x)
          (unless (gethash x seen)
            (puthash x t seen)
            (push (car x) stack)
            (push (cdr x) stack)))
         ((vectorp x)
          (mapc (lambda (e) (push e stack)) x)))))))

(defun xtask-compile-deps--file (source)
  "Return the absolute files the compiler loads while compiling SOURCE."
  (let ((names nil)
        (seen (make-hash-table :test #'eq)))
    (with-temp-buffer
      (insert-file-contents source)
      (let ((read-symbol-shorthands (progn (hack-read-symbol-shorthands)
                                           read-symbol-shorthands))
            (push (lambda (name) (push name names))))
        (goto-char (point-min))
        (condition-case nil
            (while t
              (let ((form (read (current-buffer))))
                (xtask-compile-deps--walk-toplevel form push)
                (xtask-compile-deps--walk-symbols form push seen)))
          ((end-of-file invalid-read-syntax error) nil))))
    (let ((files nil))
      (dolist (name names)
        (let ((file (xtask-compile-deps--locate name)))
          (when file (push file files))))
      (sort (delete-dups files) #'string<))))

(defun xtask-compile-deps--install-autoloads ()
  "Define the autoloads of every *loaddefs.el file on `load-path'.
A package's own loaddefs (cl-loaddefs.el, org-loaddefs.el, ...) is only
loaded with the package, so its macros are not autoloads in the dumped
image.  Evaluate just its `autoload' forms and its `compiler-macro' and
`byte-optimizer' properties: nothing else of the package runs."
  (dolist (dir (delete-dups (mapcar #'file-truename
                                    (seq-filter #'file-directory-p load-path))))
    (dolist (file (directory-files dir t "loaddefs\\.el\\'"))
      (unless (string-suffix-p "loaddefs-gen.el" file)
        (with-temp-buffer
          (insert-file-contents file)
          (goto-char (point-min))
          (condition-case nil
              (while t
                (let ((form (read (current-buffer))))
                  (pcase form
                    (`(autoload (quote ,(and sym (pred symbolp))) . ,_)
                     (unless (fboundp sym) (eval form t)))
                    (`(,(or 'put 'function-put) (quote ,(pred symbolp))
                       (quote ,(or 'compiler-macro 'byte-optimizer)) . ,_)
                     (eval form t)))))
            ((end-of-file invalid-read-syntax error) nil)))))))

(defun xtask-compile-deps-batch (input output)
  "Write the compile-time loads of the sources listed in INPUT to OUTPUT.
INPUT holds one absolute source file name per line."
  (xtask-compile-deps--install-autoloads)
  (let ((sources (with-temp-buffer
                   (insert-file-contents input)
                   (split-string (buffer-string) "\n" t)))
        (lines nil))
    (dolist (source sources)
      (let ((truename (file-truename source)))
        (dolist (dep (xtask-compile-deps--file source))
          (unless (equal dep truename)
            (push (concat source "\t" dep "\n") lines)))))
    (with-temp-file output
      (set-buffer-multibyte nil)
      (dolist (line (nreverse lines))
        (insert (encode-coding-string line 'utf-8-unix))))))

;;; compile-deps.el ends here
