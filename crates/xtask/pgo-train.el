;;; pgo-train.el --- training workload for `fresh-build --profile release-pgo'  -*- lexical-binding: t; -*-

;; Drives the paths a PGO build should optimise for. Committed (rather than
;; left to whatever benchmark happens to be lying around) so the profile is
;; reproducible and reviewable: a PGO profile bakes in assumptions about what
;; is hot, and code on unprofiled paths gets pessimised, so what is trained on
;; is part of the build's semantics.
;;
;; Deliberately covers MORE than the editing benchmarks used to discover the
;; win. Training only on font-lock measured better on font-lock (-24%) but
;; risks biasing against everything else; byte-compilation and startup are
;; included so the common non-editing paths keep their profile too.
;;
;; Batch breadth is followed by a separate real TTY session of this same
;; instrumented executable. Its private PTY driver sends actual keys and
;; verifies command acknowledgements and restored buffers. Training inputs
;; are owned here, independently of held-out performance fixtures.
;; This interactive corpus requires a POSIX PTY, Python 3, cat and git.
;; A native Windows trainer needs a separate console driver before PGO is
;; recommended there; ordinary non-PGO release builds never run this file.
;;
;; xtask runs two independent processes: the existing default-JIT corpus and
;; a fixed general-compute VM corpus.  NEOMACS_PGO_TRAIN_PASS selects "default"
;; (also the absent-selector default) or "vm".  The latter requires NEOVM_JIT=0
;; before process startup; JIT policy cannot be changed by setenv after loading.
;; Neither pass reads held-out benchmark inputs or adjusts its repetition count.

(defvar nm-pgo--phases nil)
(defvar nm-pgo--directory nil)

(defun nm-pgo--phase (name function &optional optional)
  "Record successful NAME, or explicitly report an OPTIONAL missing phase."
  (condition-case err
      (let ((result (funcall function)))
        (push `((phase . ,name) (status . "complete") (result . ,result))
              nm-pgo--phases)
        result)
    (error
     (push `((phase . ,name) (status . "failed")
             (error . ,(error-message-string err))) nm-pgo--phases)
     (unless optional (signal (car err) (cdr err)))
     (message "pgo-train: optional %s unavailable: %s" name
              (error-message-string err)))))

(defun nm-pgo--write-result (name result)
  (require 'json)
  (with-temp-file (expand-file-name name nm-pgo--directory)
    (insert (json-encode result))))

(defun nm-pgo--edit-pass (file mode-fn iters)
  "Fontify and edit around FILE the way interactive editing does."
  (let ((buf (find-file-noselect file)))
    (with-current-buffer buf
      (funcall mode-fn)
      (font-lock-set-defaults)
      (let* ((sz (buffer-size))
             (step (max 1 (/ sz (max 1 iters)))))
        (dotimes (i iters)
          (let ((pos (min (max (point-min) (* i step))
                          (max (point-min) (- (point-max) 2)))))
            (goto-char pos)
            (beginning-of-line)
            (let* ((win-start (point))
                   (win-end (save-excursion (forward-line 50) (point))))
              (font-lock-unfontify-region win-start win-end)
              (font-lock-fontify-region win-start win-end)
              (goto-char win-start)
              (insert "x")
              (let ((ins (point)))
                (font-lock-fontify-region (line-beginning-position)
                                          (line-end-position))
                (syntax-ppss ins)
                (delete-region (1- ins) ins))))))
      (set-buffer-modified-p nil)
      (kill-buffer buf))))

(defun nm-pgo--org-pass (iters)
  "Same shape as `nm-pgo--edit-pass' on a generated org buffer.
Org exercises a different frontier (Lisp-heavy fontification, text
properties, GC) than an elisp buffer does."
  (let ((buf (get-buffer-create "*pgo-org*")))
    (with-current-buffer buf
      (erase-buffer)
      (dotimes (n 80)
        (insert (format "* Heading %d :tag:\n" n))
        (insert "Text with *bold*, /italic/, =code= and a [[https://e.com][link]].\n")
        (insert "#+begin_src emacs-lisp\n(defun f (x) (* x x))\n#+end_src\n")
        (insert "| a | 1 |\n|---+---|\n| b | 2 |\n\n"))
      (when (fboundp 'org-mode) (org-mode))
      (font-lock-set-defaults)
      (let* ((sz (buffer-size))
             (step (max 1 (/ sz (max 1 iters)))))
        (dotimes (i iters)
          (let ((pos (min (max (point-min) (* i step))
                          (max (point-min) (- (point-max) 2)))))
            (goto-char pos)
            (beginning-of-line)
            (let* ((win-start (point))
                   (win-end (save-excursion (forward-line 50) (point))))
              (font-lock-unfontify-region win-start win-end)
              (font-lock-fontify-region win-start win-end)))))
      (kill-buffer buf))))

(defun nm-pgo--byte-compile-pass (files)
  "Byte-compile FILES into a scratch dir, then discard it.
Byte-compilation is the other workload users wait on, and it stresses
the reader, macro expansion and the compiler rather than redisplay."
  (let ((dir (make-temp-file "nm-pgo-bc" t)))
    (unwind-protect
        (dolist (f files)
          (when (file-readable-p f)
            (let ((copy (expand-file-name (file-name-nondirectory f) dir)))
              (copy-file f copy t)
              (byte-compile-file copy))))
      (delete-directory dir t))))


(defun nm-pgo--search-pass (file iters)
  "Regexp and literal search, plus replace -- the search engine and its
case-translation tables, which fontification alone barely touches."
  (let ((buf (find-file-noselect file)))
    (with-current-buffer buf
      (dotimes (_ iters)
        (goto-char (point-min))
        (while (re-search-forward "(defun \\([a-z-]+\\)" nil t)
          (match-string 1))
        (goto-char (point-min))
        (while (search-forward "let" nil t))
        (let ((case-fold-search t))
          (goto-char (point-min))
          (while (re-search-forward "[A-Z][a-z]+" nil t))))
      (set-buffer-modified-p nil)
      (kill-buffer buf))))

(defun nm-pgo--text-pass ()
  "Buffer mutation: insert/delete/kill/yank/undo and markers -- the edit
primitives every command goes through, distinct from fontification."
  (let ((buf (get-buffer-create "*pgo-text*")))
    (with-current-buffer buf
      (erase-buffer)
      (buffer-enable-undo)
      (dotimes (i 400)
        (insert (format "line %d with some words to move around\n" i)))
      (dotimes (_ 30)
        (goto-char (point-min))
        (kill-line 5) (goto-char (point-max)) (yank)
        (goto-char (point-min))
        (forward-word 20) (set-mark (point)) (forward-word 10)
        (upcase-region (region-beginning) (region-end))
        (undo-boundary))
      (dotimes (_ 10) (ignore-errors (undo)))
      (kill-buffer buf))))

(defun nm-pgo--mode-pass (files)
  "Fontify buffers in OTHER major modes. Without this, every mode a user
opens that is not elisp or org is laid out as cold code."
  (dolist (f files)
    (when (file-readable-p f)
      (let ((buf (find-file-noselect f)))
        (with-current-buffer buf
          (normal-mode)
          (font-lock-set-defaults)
          (font-lock-fontify-region (point-min) (min (point-max) 60000))
          (set-buffer-modified-p nil))
        (kill-buffer buf)))))

(defun nm-pgo--lisp-data-pass ()
  "Reader, printer, sorting, hash tables and string formatting -- the
general-purpose Lisp machinery under every command."
  (let ((data nil))
    (dotimes (i 2000)
      (push (cons (format "key-%d" i) (list i (* i i) (number-to-string i))) data))
    (setq data (sort data (lambda (a b) (string< (car a) (car b)))))
    (let ((h (make-hash-table :test #'equal)))
      (dolist (d data) (puthash (car d) (cdr d) h))
      (dolist (d data) (gethash (car d) h)))
    (dotimes (_ 20)
      (car (read-from-string (prin1-to-string data))))))

(defun nm-pgo--count-records (records)
  (let ((count 0))
    (while (consp records)
      (setq count (1+ count) records (cdr records)))
    count))

(defun nm-pgo--record-width (record)
  (+ (length (car record)) (cdr record)))

(defun nm-pgo--index-total (limit)
  (let ((index 0) (total 0))
    (while (< index limit)
      (setq total (+ total index) index (1+ index)))
    total))

(defun nm-pgo--bytecode-execution-pass ()
  "Execute private bytecode with ordinary default tier/OSR policy."
  (require 'bytecomp)
  (dolist (name '(nm-pgo--count-records nm-pgo--record-width nm-pgo--index-total))
    (fset name (byte-compile (symbol-function name)))
    (unless (byte-code-function-p (symbol-function name))
      (error "Training helper %s was not byte-compiled" name)))
  ;; Default retiering is sixteen times the 1000-call heat threshold.
  ;; A normal repeated data pass crosses it without changing runtime policy.
  (let ((records nil) (calls 32000))
    (dotimes (i 64) (push (cons (format "record-%03d" i) i) records))
    (dotimes (i calls)
      (unless (= (nm-pgo--count-records records) 64)
        (error "Bytecode traversal result differs"))
      (let* ((record (nth (% i 64) records))
             (expected (+ 10 (cdr record))))
        (unless (= (nm-pgo--record-width record) expected)
          (error "Bytecode record result differs"))))
    (unless (= (nm-pgo--index-total 70000) 2449965000)
      (error "Bytecode loop result differs"))
    `((helpers . 3) (calls_each . ,calls) (records . 64) (loop_iterations . 70000))))

(defun nm-pgo--vm-list (records)
  "Traverse ordinary labelled records with data-dependent branches."
  (let ((count 0) (total 0) (even-total 0))
    (while (consp records)
      (let ((value (car (car records))))
        (setq count (1+ count) total (+ total value))
        (when (= (% value 2) 0)
          (setq even-total (+ even-total value))))
      (setq records (cdr records)))
    (vector count total even-total)))

(defun nm-pgo--vm-vector (values)
  "Transform a vector, retaining and checking the resulting values."
  (let* ((size (length values)) (output (make-vector size 0)) (total 0))
    (dotimes (index size)
      (let ((value (logand (+ (* (aref values index) 3) 7) 255)))
        (aset output index value)
        (setq total (+ total value))))
    (vector (length output) total (aref output 0) (aref output (1- size)))))

(defun nm-pgo--vm-arithmetic (limit)
  "Exercise ordinary integer arithmetic and exactly representable floats."
  (let ((index 0) (total 0) (remainders 0) (quotients 0) (halves 0.0))
    (while (< index limit)
      (setq total (+ total index)
            remainders (+ remainders (% index 7))
            quotients (+ quotients (/ index 7))
            halves (+ halves (* index 0.5))
            index (1+ index)))
    (vector total remainders quotients halves)))

(defun nm-pgo--vm-closures (values offset)
  "Invoke two bytecompiled closures over different captured lexical cells."
  (let* ((other-offset (- offset 10))
         (up (lambda (value) (+ value offset)))
         (down (lambda (value) (+ value other-offset)))
         (up-total 0) (down-total 0))
    (unless (and (byte-code-function-p up) (byte-code-function-p down))
      (error "VM training closures were not byte-compiled"))
    (dolist (value values)
      (setq up-total (+ up-total (funcall up value))
            down-total (+ down-total (funcall down value))))
    (vector up-total down-total)))

(defun nm-pgo--vm-sequences (values)
  "Run bundled sequence Lisp over both list and vector inputs."
  (let* ((mapped (seq-map #'1+ values))
         (selected (seq-filter (lambda (value) (= (% value 2) 0)) mapped)))
    (vector (length selected) (seq-reduce #'+ selected 0))))

(defun nm-pgo--vm-strings (text expected)
  "Parse ASCII or Unicode records and verify their complete UTF8 roundtrip."
  (let* ((words (split-string (string-trim text) "[ \t]+" t))
         (joined (string-join words "|"))
         (encoded (encode-coding-string joined 'utf-8))
         (decoded (decode-coding-string encoded 'utf-8)))
    (unless (and (equal joined expected) (equal decoded expected))
      (error "VM training string contents differ"))
    (vector (length words) (length joined) (string-bytes encoded))))

(defun nm-pgo--vm-run (helper function expected checksum)
  "Check 8192 calls of FUNCTION against EXPECTED and aggregate CHECKSUM.
FUNCTION receives the call index.  EXPECTED is a complete result vector or
a function of that index, allowing fixed alternating input representations."
  (let ((total 0))
    (dotimes (index 8192)
      (let ((result (funcall function index))
            (wanted (if (functionp expected) (funcall expected index) expected)))
        (unless (equal result wanted)
          (error "VM training %s call %d differs: %S versus %S"
                 helper index result wanted))
        (setq total (+ total (aref result 0)))))
    (unless (= total checksum)
      (error "VM training %s aggregate checksum differs" helper))
    `((helper . ,(symbol-name helper)) (calls . 8192)
      (checksum . ,total) (expected_checksum . ,checksum) (results_verified . t))))

(defun nm-pgo--vm-pass ()
  "Fixed VM-only compute corpus; no editing or benchmark fixture inputs."
  (require 'bytecomp)
  (require 'seq)
  (require 'subr-x)
  (dolist (name '(nm-pgo--vm-list nm-pgo--vm-vector nm-pgo--vm-arithmetic
                  nm-pgo--vm-closures nm-pgo--vm-sequences nm-pgo--vm-strings))
    (fset name (byte-compile (symbol-function name)))
    (unless (byte-code-function-p (symbol-function name))
      (error "VM training helper %s was not byte-compiled" name)))
  (let* ((values (number-sequence 0 63))
         (array (vconcat values))
         (records (mapcar (lambda (value)
                            (cons value (format "record-%03d" value))) values))
         (texts ["  record ab cafe / item 42  " "  record αβ café / item 42  "])
         (joined ["record|ab|cafe|/|item|42" "record|αβ|café|/|item|42"]))
    (nm-pgo--phase "vm-lists"
                  (lambda () (nm-pgo--vm-run
                              'nm-pgo--vm-list
                              (lambda (_index) (nm-pgo--vm-list records))
                              [64 2016 992] 524288)))
    (nm-pgo--phase "vm-vectors"
                  (lambda () (nm-pgo--vm-run
                              'nm-pgo--vm-vector
                              (lambda (_index) (nm-pgo--vm-vector array))
                              [64 6496 7 196] 524288)))
    (nm-pgo--phase "vm-arithmetic"
                  (lambda () (nm-pgo--vm-run
                              'nm-pgo--vm-arithmetic
                              (lambda (_index) (nm-pgo--vm-arithmetic 64))
                              [2016 189 261 1008.0] 16515072)))
    (nm-pgo--phase "vm-closures"
                  (lambda () (nm-pgo--vm-run
                              'nm-pgo--vm-closures
                              (lambda (_index) (nm-pgo--vm-closures values 7))
                              [2464 1824] 20185088)))
    (nm-pgo--phase "vm-bundled-sequences"
                  (lambda () (nm-pgo--vm-run
                              'nm-pgo--vm-sequences
                              (lambda (index)
                                (nm-pgo--vm-sequences
                                 (if (= (% index 2) 0) values array)))
                              [32 1056] 262144)))
    (nm-pgo--phase "vm-bundled-strings"
                  (lambda () (nm-pgo--vm-run
                              'nm-pgo--vm-strings
                              (lambda (index)
                                (let ((kind (% index 2)))
                                  (nm-pgo--vm-strings (aref texts kind)
                                                      (aref joined kind))))
                              (lambda (index)
                                (if (= (% index 2) 0) [6 24 24] [6 24 27]))
                              49152)))))

(defun nm-pgo--command-output (program &rest arguments)
  (with-temp-buffer
    (let ((status (apply #'call-process program nil t nil arguments)))
      (unless (equal status 0)
        (error "Training subprocess %s failed (%S): %s"
               program status (buffer-string)))
      (buffer-string))))

(defun nm-pgo--process-pass ()
  "Exercise a local pipe filter and ordinary scratch-repository output."
  (let* ((program (or (executable-find "cat") (error "cat required for training")))
         (buffer (generate-new-buffer " *pgo-process*"))
         (text (mapconcat
                (lambda (n) (format "file-%03d.el: modified λ record %d\n" n n))
                (number-sequence 0 63) ""))
         (received "") (pending "") (lines 0)
         (finished nil) (exit-status nil) process
         (deadline (+ (float-time) 30)))
    (unwind-protect
        (progn
          (setq process
                (make-process
                 :name "pgo-training-pipe" :buffer nil :noquery t
                 :command (list program) :connection-type 'pipe :coding 'utf-8-unix
                 :filter
                 (lambda (_process chunk)
                   (setq received (concat received chunk)
                         pending (concat pending chunk))
                   (while (string-match "\n" pending)
                     (let* ((end (match-end 0)) (line (substring pending 0 end)))
                       (setq pending (substring pending end))
                       (unless (string-match "^file-[0-9]+\\.el: modified" line)
                         (error "Unexpected training process record"))
                       (setq lines (1+ lines))
                       (with-current-buffer buffer
                         (goto-char (point-max))
                         (let ((start (point)))
                           (insert line)
                           (put-text-property start (point) 'face 'font-lock-string-face))))))
                 :sentinel
                 (lambda (child _event)
                   (when (memq (process-status child) '(exit signal))
                     (setq exit-status (process-exit-status child) finished t)))))
          ;; Distinct writes require correct incremental decoding/line joining,
          ;; without assuming any particular OS pipe chunk boundaries.
          (let ((middle (/ (length text) 2)))
            (process-send-string process (substring text 0 middle))
            (process-send-string process (substring text middle)))
          (process-send-eof process)
          (while (and (< (float-time) deadline)
                      (or (process-live-p process) (not finished)))
            (accept-process-output process 0.05))
          (unless (and finished (equal exit-status 0) (= lines 64)
                       (equal pending "") (equal received text)
                       (equal (with-current-buffer buffer (buffer-string)) text))
            (error "Incomplete process training: exit=%S lines=%d" exit-status lines)))
      (when (and process (process-live-p process)) (delete-process process))
      (kill-buffer buffer)))
  (let* ((git (or (executable-find "git") (error "git required for training")))
         (repo (expand-file-name "repository" nm-pgo--directory))
         (file (expand-file-name "notes.txt" repo))
         (process-environment
          (append '("GIT_CONFIG_NOSYSTEM=1" "GIT_CONFIG_GLOBAL=/dev/null"
                    "GIT_TERMINAL_PROMPT=0")
                  (let ((rest process-environment) (clean nil))
                    (while rest
                      (unless (string-match
                               "^GIT_\\(DIR\\|WORK_TREE\\|INDEX_FILE\\|OBJECT_DIRECTORY\\|COMMON_DIR\\|NAMESPACE\\|CONFIG_COUNT\\)="
                               (car rest))
                        (push (car rest) clean))
                      (setq rest (cdr rest)))
                    (nreverse clean)))))
    (nm-pgo--command-output git "init" "--quiet" repo)
    (with-temp-file file
      (dotimes (n 48) (insert (format "record %02d: original text\n" n))))
    (nm-pgo--command-output git "-C" repo "add" "notes.txt")
    (nm-pgo--command-output git "-C" repo "-c" "user.name=PGO Training"
                            "-c" "user.email=pgo@example.invalid"
                            "-c" "commit.gpgsign=false" "-c" "core.hooksPath=/dev/null"
                            "commit" "--quiet" "-m" "training seed")
    (with-temp-file file
      (dotimes (n 48)
        (insert (format "record %02d: %s text\n" n
                        (if (= (% n 9) 0) "updated" "original")))))
    (let ((status (nm-pgo--command-output git "-C" repo "status" "--porcelain=v1"))
          (diff (nm-pgo--command-output git "-C" repo "--no-pager" "diff"
                                        "--no-ext-diff" "--no-color")))
      (unless (and (string-match "notes\\.txt" status)
                   (string-match "^@@ " diff) (string-match "updated text" diff))
        (error "Missing scratch repository changes"))
      (with-temp-buffer
        (insert diff)
        (goto-char (point-min))
        (while (re-search-forward "^\\([+-]\\)record \\([0-9]+\\):" nil t)
          (put-text-property (match-beginning 0) (match-end 0)
                             'pgo-change (match-string 1))))
      `((pipe_records . 64) (status_bytes . ,(string-bytes status))
        (diff_bytes . ,(string-bytes diff))))))

(defvar nm-pgo--tty-buffers nil)
(defvar nm-pgo--tty-checksums nil)
(defvar nm-pgo--tty-commands 0)

(defun nm-pgo--tty-marker (text)
  (send-string-to-terminal (concat "\e]777;NMPGO;" text "\a")))

(defun nm-pgo--tty-fail (err)
  (nm-pgo--tty-marker (format "ERROR;%S" err))
  (kill-emacs 2))

(defun nm-pgo--tty-search ()
  (interactive)
  (unless (search-forward "record" nil t)
    (goto-char (point-min))
    (search-forward "record")))

(defun nm-pgo--tty-switch ()
  (interactive)
  (switch-to-buffer (if (eq (current-buffer) (car nm-pgo--tty-buffers))
                        (cadr nm-pgo--tty-buffers) (car nm-pgo--tty-buffers))))

(defun nm-pgo--tty-post-command ()
  (condition-case err
      (progn
        (unless (memq (current-buffer) nm-pgo--tty-buffers)
          (error "Command left the training buffers"))
        (setq nm-pgo--tty-commands (1+ nm-pgo--tty-commands))
        (redisplay t)
        (nm-pgo--tty-marker
         (format "ACK;%d;%s" nm-pgo--tty-commands this-command)))
    (error (nm-pgo--tty-fail err))))

(defun nm-pgo--tty-finish ()
  (interactive)
  (remove-hook 'post-command-hook #'nm-pgo--tty-post-command)
  (condition-case err
      (progn
        (dolist (item nm-pgo--tty-checksums)
          (unless (equal (cdr item) (secure-hash 'sha256 (car item)))
            (error "TTY input changed final buffer contents")))
        (nm-pgo--write-result
         "tty-result.json"
         `((status . "complete") (commands . ,nm-pgo--tty-commands)
           (buffers_restored . t) (buffers . 2)))
        (kill-emacs 0))
    (error (nm-pgo--tty-fail err))))

(defun nm-pgo--tty-setup ()
  (condition-case err
      (progn
        (setq inhibit-startup-screen t inhibit-startup-message t)
        (when (fboundp 'menu-bar-mode) (menu-bar-mode -1))
        (dotimes (kind 2)
          (let ((buffer (get-buffer-create (format "*pgo-training-%d*" kind))))
            (with-current-buffer buffer
              (erase-buffer)
              (dotimes (n 180)
                (insert (if (= kind 0)
                            (format "(message \"record %03d λ café\") ; local notes\n" n)
                          (concat (format "* record %03d: heading with unicode Ελληνικά " n)
                                  "and ordinary words in a paragraph that wraps over the terminal width while retaining readable text.\n"))))
              (if (= kind 0) (emacs-lisp-mode) (text-mode))
              (setq truncate-lines nil)
              (font-lock-set-defaults)
              (font-lock-ensure (point-min) (point-max))
              (local-set-key "\177" #'delete-backward-char)
              (local-set-key "\C-cs" #'nm-pgo--tty-search)
              (local-set-key "\C-cb" #'nm-pgo--tty-switch)
              (local-set-key "\C-cq" #'nm-pgo--tty-finish)
              (goto-char (point-min)) (forward-line 45)
              (push buffer nm-pgo--tty-buffers)
              (push (cons buffer (secure-hash 'sha256 buffer)) nm-pgo--tty-checksums))))
        (switch-to-buffer (car nm-pgo--tty-buffers))
        (message nil)
        (redisplay t)
        (add-hook 'post-command-hook #'nm-pgo--tty-post-command t)
        (nm-pgo--tty-marker "READY"))
    (error (nm-pgo--tty-fail err))))

(defun nm-pgo--tty-pass (fixture)
  (let* ((editor (expand-file-name invocation-name invocation-directory))
         (driver (expand-file-name "pgo-tty-train.py" (file-name-directory fixture)))
         (output (expand-file-name "tty" nm-pgo--directory)))
    (unless (file-executable-p editor) (error "Cannot resolve invoked editor %s" editor))
    (nm-pgo--command-output "python3" driver "--editor" editor
                            "--fixture" fixture "--output" output)
    `((driver_result . ,(expand-file-name "driver-result.json" output)))))

(let* ((fixture (expand-file-name (or load-file-name buffer-file-name)))
       (root (or (getenv "NEOMACS_RUNTIME_ROOT") default-directory))
       (pass (or (getenv "NEOMACS_PGO_TRAIN_PASS") "default"))
       (tty (equal (getenv "NEOMACS_PGO_TTY") "1")))
  (unless (member pass '("default" "vm"))
    (error "Unknown NEOMACS_PGO_TRAIN_PASS: %S" pass))
  (if (equal pass "vm")
      (unless (and (equal (getenv "NEOVM_JIT") "0") (not tty))
        (error "VM training requires NEOVM_JIT=0 and no TTY mode"))
    (when (getenv "NEOVM_JIT")
      (error "Default training requires NEOVM_JIT unset")))
  (if tty
      (progn
        (setq nm-pgo--directory (or (getenv "NEOMACS_PGO_TRAIN_DIR")
                                    (error "TTY training directory missing")))
        (run-at-time 0.1 nil #'nm-pgo--tty-setup))
    (let* ((scratch (or (getenv "NEOMACS_PGO_TRAIN_ROOT")
                        (expand-file-name "tmp/pgo-training" default-directory)))
           (el (expand-file-name "lisp/emacs-lisp/cl-macs.el" root))
           (bc (mapcar (lambda (n) (expand-file-name (concat "lisp/emacs-lisp/" n) root))
                       '("seq.el" "map.el" "pcase.el" "rx.el"))))
      (make-directory scratch t)
      (setq nm-pgo--directory (make-temp-file (expand-file-name "run-" scratch) t))
      (if (equal pass "vm")
          (progn
            (let ((temporary-file-directory nm-pgo--directory))
              (nm-pgo--vm-pass))
            (nm-pgo--write-result
             "vm-result.json"
             `((status . "complete") (pass . "vm") (jit . "0")
               (rounds . 8192) (helpers . 6)
               (phases . ,(vconcat (reverse nm-pgo--phases))))))
        (progn
          (let ((temporary-file-directory nm-pgo--directory))
            (nm-pgo--phase "elisp-edit" (lambda ()
                                          (unless (file-readable-p el) (error "Missing %s" el))
                                          (nm-pgo--edit-pass el #'emacs-lisp-mode 60)))
            (nm-pgo--phase "org-fontification" (lambda () (require 'org) (nm-pgo--org-pass 40)) t)
            (nm-pgo--phase "byte-compile" (lambda () (nm-pgo--byte-compile-pass bc)))
            (nm-pgo--phase "search" (lambda () (nm-pgo--search-pass el 3)))
            (nm-pgo--phase "text" #'nm-pgo--text-pass)
            (nm-pgo--phase "lisp-data" #'nm-pgo--lisp-data-pass)
            (nm-pgo--phase "other-source-files"
                          (lambda () (nm-pgo--mode-pass
                                      (mapcar (lambda (n) (expand-file-name n root))
                                              '("lisp/progmodes/python.el" "lisp/progmodes/cc-mode.el"
                                                "lisp/dired.el" "lisp/net/tramp.el")))) t)
            (nm-pgo--phase "bytecode-execution" #'nm-pgo--bytecode-execution-pass)
            (nm-pgo--phase "process-io" #'nm-pgo--process-pass)
            (nm-pgo--phase "tty-input-redisplay" (lambda () (nm-pgo--tty-pass fixture))))
          (nm-pgo--write-result "batch-result.json"
                               `((status . "complete") (phases . ,(vconcat (reverse nm-pgo--phases)))))))
      (message "pgo-train: done; evidence %s" nm-pgo--directory))))
