//! GNU-oracle locks for Green Lisp threads (Phase 0 step 2, round 3d): GNU's
//! thread primitives with `NEOVM_THREADS=green`, each Lisp thread on its own
//! OS thread. Ported from the F4 research cases (`oracle_facade_switch.rs`)
//! plus the 3d cases.
//!
//! GNU picks which thread runs after a `thread-yield` by OS scheduling, so
//! every form here reaches its answer through joins, mutexes, conditions or
//! waits for a flag, never through one particular interleaving.
//!
//! Refresh empty expectations from GNU 31.1 with
//! `NEOVM_ORACLE_MODE=refresh UPDATE_EXPECT=1`; never hand-write answers.
//! Run this generated Lisp only through the mandatory PID sandbox.

#[path = "../src/common.rs"]
mod common;

use common::return_if_neovm_enable_oracle_proptest_not_set;

/// Green threads, which refuse a JIT-enabled session (round 3d2).
const GREEN_THREADS: &[(&str, &str)] = &[("NEOVM_THREADS", "green"), ("NEOVM_JIT", "0")];

fn assert_green_oracle(form: &str, expected: expect_test::Expect) {
    common::assert_oracle_parity_under_envs_expect(form, &[GREEN_THREADS], expected);
}

#[test]
fn green_thread_identity_name_liveness_and_self_join() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let ((worker
       (make-thread
        (lambda ()
          (list (threadp (current-thread))
                (eq (current-thread) (current-thread))
                (thread-name (current-thread))
                (condition-case nil (thread-join (current-thread))
                  (error 'self-join-error))))
        "g3d-reflection")))
  (list (threadp worker) (thread-name worker)
        (thread-join worker) (thread-live-p worker)
        (length (all-threads)) (eq (car (all-threads)) main-thread)))
"##,
        expect_test::expect![[
            r#""OK (t \"g3d-reflection\" (t t \"g3d-reflection\" self-join-error) nil 1 t)""#
        ]],
    );
}

#[test]
fn green_ping_pong_dynamic_bindings() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (defvar neovm-g3d-special)
  (setq neovm-g3d-special 'canonical)
  (let* ((turn 'main)
         (trace nil)
         (worker
          (make-thread
           (lambda ()
             (let ((neovm-g3d-special 'worker))
               (dotimes (round 4)
                 (while (not (eq turn 'worker)) (thread-yield))
                 (push (list 'worker round neovm-g3d-special) trace)
                 (setq turn 'main)
                 (thread-yield))
               neovm-g3d-special))
           "g3d-ping-pong")))
    (let ((neovm-g3d-special 'main))
      (dotimes (round 4)
        (while (not (eq turn 'main)) (thread-yield))
        (push (list 'main round neovm-g3d-special) trace)
        (setq turn 'worker)
        (thread-yield))
      (while (not (eq turn 'main)) (thread-yield))
      (list (nreverse trace) (thread-join worker)
            neovm-g3d-special (default-value 'neovm-g3d-special)))))
"##,
        expect_test::expect![[
            r#""OK (((main 0 main) (worker 0 worker) (main 1 main) (worker 1 worker) (main 2 main) (worker 2 worker) (main 3 main) (worker 3 worker)) worker main main)""#
        ]],
    );
}

#[test]
fn green_new_thread_sees_global_values_not_its_creators_let() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (defvar neovm-g3d-global 0)
  (let ((neovm-g3d-global 1))
    (list neovm-g3d-global
          (thread-join (make-thread (lambda () neovm-g3d-global)))
          (thread-join (make-thread (lambda ()
                                      (let ((neovm-g3d-global 2))
                                        (thread-yield)
                                        neovm-g3d-global))))
          neovm-g3d-global
          (default-value 'neovm-g3d-global))))
"##,
        expect_test::expect![[r#""OK (1 0 2 1 1)""#]],
    );
}

#[test]
fn green_parked_binding_survives_worker_gc() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (defvar neovm-g3d-special)
  (setq neovm-g3d-special 'canonical)
  (let* ((collect-now nil)
         (collected nil)
         (worker
          (make-thread
           (lambda ()
             (while (not collect-now) (thread-yield))
             (dotimes (i 64) (make-list 256 i))
             (garbage-collect)
             (setq collected t)
             neovm-g3d-special)
           "g3d-collector")))
    (let ((neovm-g3d-special (list 'owner-a (make-string 48 ?a))))
      (setq collect-now t)
      (while (not collected) (thread-yield))
      (list (thread-join worker)
            (car neovm-g3d-special)
            (length (cadr neovm-g3d-special))
            (thread-live-p worker)))))
"##,
        expect_test::expect![[r#""OK (canonical owner-a 48 nil)""#]],
    );
}

#[test]
fn green_join_internal_error_is_last_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (thread-last-error t)
  (let* ((worker
          (make-thread (lambda () (signal 'error '(g3d-worker)))
                       "g3d-internal-error"))
         (joined (condition-case err (thread-join worker)
                   (error (list 'join-error err)))))
    (list joined (thread-live-p worker) (thread-last-error t)
          (thread-last-error))))
"##,
        expect_test::expect![[r#""OK (nil nil (error g3d-worker) nil)""#]],
    );
}

#[test]
fn green_recursive_mutex_across_yields() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((mutex (make-mutex "g3d-recursive"))
       (ready nil)
       (entered nil)
       (worker nil))
  (mutex-lock mutex)
  (mutex-lock mutex)
  (setq worker
        (make-thread
         (lambda ()
           (setq ready t)
           (mutex-lock mutex)
           (unwind-protect
               (progn (setq entered t) 'acquired)
             (mutex-unlock mutex)))
         "g3d-contender"))
  (while (not ready) (thread-yield))
  (mutex-unlock mutex)
  (thread-yield)
  (let ((after-first-unlock entered))
    (mutex-unlock mutex)
    (list after-first-unlock (thread-join worker) entered
          (condition-case err (mutex-unlock mutex) (error err)))))
"##,
        expect_test::expect![[
            r#""OK (nil acquired t (error \"Cannot unlock mutex owned by another thread\"))""#
        ]],
    );
}

#[test]
fn green_mutex_unlock_by_another_thread_is_an_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let ((mutex (make-mutex)))
  (mutex-lock mutex)
  (list (thread-join
         (make-thread (lambda ()
                        (condition-case err (mutex-unlock mutex)
                          (error err)))))
        (mutex-unlock mutex)))
"##,
        expect_test::expect![[
            r#""OK ((error \"Cannot unlock mutex owned by another thread\") nil)""#
        ]],
    );
}

#[test]
fn green_condition_wait_notify() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((mutex (make-mutex "g3d-condition-mutex"))
       (condition (make-condition-variable mutex "g3d-condition"))
       (ready nil)
       (go nil)
       (trace nil)
       (worker
        (make-thread
         (lambda ()
           (mutex-lock mutex)
           (unwind-protect
               (progn
                 (setq ready t)
                 (while (not go) (condition-wait condition))
                 (push 'worker-awake trace)
                 'notified)
             (mutex-unlock mutex)))
         "g3d-condition-worker")))
  (while (not ready) (thread-yield))
  (mutex-lock mutex)
  (unwind-protect
      (progn
        (push 'main-before-notify trace)
        (setq go t)
        (condition-notify condition)
        (push 'main-after-notify trace))
    (mutex-unlock mutex))
  (list (thread-join worker)
        (not (null (memq 'main-before-notify trace)))
        (not (null (memq 'main-after-notify trace)))
        (not (null (memq 'worker-awake trace)))
        (condition-name condition)
        (eq (condition-mutex condition) mutex)))
"##,
        expect_test::expect![[r#""OK (notified t t t \"g3d-condition\" t)""#]],
    );
}

#[test]
fn green_condition_wait_keeps_the_recursive_count_and_notify_all() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((mutex (make-mutex))
       (condition (make-condition-variable mutex))
       (waiting 0)
       (go nil)
       (workers
        (mapcar
         (lambda (n)
           (make-thread
            (lambda ()
              (mutex-lock mutex)
              (mutex-lock mutex)
              (setq waiting (1+ waiting))
              (while (not go) (condition-wait condition))
              (list n (mutex-unlock mutex) (mutex-unlock mutex)
                    (condition-case nil (mutex-unlock mutex)
                      (error 'released))))))
         '(1 2 3))))
  (while (< waiting 3) (thread-yield))
  (mutex-lock mutex)
  (setq go t)
  (condition-notify condition t)
  (mutex-unlock mutex)
  (mapcar #'thread-join workers))
"##,
        expect_test::expect![[
            r#""OK ((1 nil nil released) (2 nil nil released) (3 nil nil released))""#
        ]],
    );
}

#[test]
fn green_signal_parked_thread() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((ready nil)
       (cleanup nil)
       (caught nil)
       (worker
        (make-thread
         (lambda ()
           (unwind-protect
               (condition-case err
                   (progn
                     (setq ready t)
                     (while t (thread-yield)))
                 (error (setq caught err) 'handled))
             (setq cleanup t)))
         "g3d-signalled-worker")))
  (while (not ready) (thread-yield))
  (thread-signal worker 'error '(g3d-injected))
  (let ((first-join
         (condition-case err (thread-join worker)
           (error (list 'join-signal err)))))
    (while (thread-live-p worker) (thread-yield))
    (list first-join caught cleanup (thread-live-p worker))))
"##,
        expect_test::expect![[
            r#""OK ((join-signal (error g3d-injected)) (error g3d-injected) t nil)""#
        ]],
    );
}

#[test]
fn green_signal_ends_waits_for_a_mutex_and_a_condition() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((held (make-mutex))
       (cmutex (make-mutex))
       (condition (make-condition-variable cmutex))
       (blocked 0)
       (locker nil)
       (waiter nil))
  (mutex-lock held)
  (setq locker (make-thread
                (lambda ()
                  (setq blocked (1+ blocked))
                  (condition-case err (progn (mutex-lock held) 'locked)
                    (error (list 'lock err))))))
  (setq waiter (make-thread
                (lambda ()
                  (mutex-lock cmutex)
                  (setq blocked (1+ blocked))
                  (condition-case err (progn (condition-wait condition) 'notified)
                    (error (list 'wait err (mutex-unlock cmutex)))))))
  (while (or (< blocked 2)
             (not (eq (thread--blocker locker) held))
             (not (eq (thread--blocker waiter) condition)))
    (thread-yield))
  (thread-signal locker 'error '("l"))
  (thread-signal waiter 'error '("w"))
  (while (or (thread-live-p locker) (thread-live-p waiter)) (thread-yield))
  (mutex-unlock held)
  (list (thread-join locker) (thread-join waiter)
        (thread--blocker locker) (thread--blocker waiter)))
"##,
        expect_test::expect![[r#""OK ((lock (error \"l\")) (wait (error \"w\") nil) nil nil)""#]],
    );
}

#[test]
fn green_signal_before_a_thread_runs_waits_for_its_first_switch() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((log nil)
       (quiet (make-thread (lambda () (push 'quiet-ran log) 'quiet-result)))
       (yielder (make-thread
                 (lambda ()
                   (push 'yielder-ran log)
                   (condition-case err (progn (thread-yield) 'no-signal)
                     (error (list 'at-yield err)))))))
  (thread-signal quiet 'error '("q"))
  (thread-signal yielder 'error '("y"))
  ;; QUIET never switches, so its error is still pending when it ends: the
  ;; join re-raises it. YIELDER raises its error at its first switch; once
  ;; it has ended, a join finds nothing pending.
  (let ((quiet-join (condition-case err (thread-join quiet) (error (list 'join err)))))
    (while (thread-live-p yielder) (thread-yield))
    (list quiet-join
          (condition-case err (thread-join yielder) (error (list 'join err)))
          (sort (copy-sequence log) #'string<))))
"##,
        expect_test::expect![[
            r#""OK ((join (error \"q\")) (at-yield (error \"y\")) (quiet-ran yielder-ran))""#
        ]],
    );
}

#[test]
fn green_signal_to_the_main_thread_raises_nothing_in_batch() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(list (thread-join (make-thread (lambda ()
                                  (thread-signal main-thread 'error '("to-main"))
                                  'sent)))
      (condition-case err (progn (thread-yield) (sleep-for 0.01) 'main-ok)
        (error (list 'main-error err))))
"##,
        expect_test::expect![[r#""OK (sent main-ok)""#]],
    );
}

#[test]
fn green_waits_let_other_threads_run() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let ((ran nil))
  (make-thread (lambda () (push 'during-sleep-for ran)))
  (sleep-for 0.2)
  (make-thread (lambda () (push 'during-accept ran)))
  (accept-process-output nil 0.2)
  (make-thread (lambda () (push 'during-sit-for ran)))
  (sit-for 0.2)
  (nreverse ran))
"##,
        expect_test::expect![[r#""OK (during-sleep-for during-accept during-sit-for)""#]],
    );
}

#[test]
fn green_killed_current_buffer_moves_and_signals_the_thread() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((buffer (get-buffer-create "g3d-killed"))
       (mutex (make-mutex))
       (condition (make-condition-variable mutex))
       (waiting nil)
       (go nil)
       (worker
        (with-current-buffer buffer
          (make-thread
           (lambda ()
             (mutex-lock mutex)
             (setq waiting t)
             (condition-case err
                 (progn (while (not go) (condition-wait condition)) 'not-signalled)
               (error (list err (eq (current-buffer) buffer)
                            (buffer-live-p (current-buffer))
                            (prog1 (mutex-unlock mutex) nil)))))))))
  (while (not waiting) (thread-yield))
  (let ((killed (kill-buffer buffer)))
    (mutex-lock mutex)
    (setq go t)
    (condition-notify condition)
    (mutex-unlock mutex)
    (list killed (buffer-live-p buffer)
          (condition-case err (thread-join worker) (error (list 'join err)))
          (thread-join worker))))
"##,
        expect_test::expect![[
            r#""OK (t nil (join (thread-buffer-killed)) ((thread-buffer-killed) nil t nil))""#
        ]],
    );
}

#[test]
fn green_buffer_disposition_t_keeps_and_silently_hides() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let* ((kept (get-buffer-create "g3d-kept"))
       (quiet (get-buffer-create "g3d-quiet"))
       (release nil)
       (parked 0)
       (keeper (with-current-buffer kept
                 (make-thread (lambda ()
                                (setq parked (1+ parked))
                                (while (not release) (thread-yield))
                                (list (buffer-name) (thread-buffer-disposition (current-thread))))
                              "keeper" t)))
       (hider (with-current-buffer quiet
                (make-thread (lambda ()
                               (setq parked (1+ parked))
                               (condition-case err
                                   (progn (while (not release) (thread-yield))
                                          (list (eq (current-buffer) quiet)
                                                (buffer-live-p (current-buffer))))
                                 (error (list 'signalled err))))
                             "hider" 'silently))))
  (while (< parked 2) (thread-yield))
  (let ((results (list (kill-buffer kept) (buffer-live-p kept)
                       (kill-buffer quiet) (buffer-live-p quiet))))
    (setq release t)
    (append results (list (thread-join keeper) (thread-join hider)))))
"##,
        expect_test::expect![[r#""OK (nil t t nil (\"g3d-kept\" t) (nil t))""#]],
    );
}

#[test]
fn green_eight_threads_yield_gc() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (thread-last-error t)
  (defvar neovm-g3d-special)
  (setq neovm-g3d-special 'canonical)
  (let ((threads nil))
    (dotimes (index 8)
      (let ((owner index))
        (push
         (make-thread
          (lambda ()
            (let ((neovm-g3d-special owner)
                  (only-here (list owner (make-string 32 (+ ?a owner))))
                  (seed (+ owner 1)))
              (dotimes (round 24)
                (setq seed (% (+ (* seed 1103515245) 12345) 2147483647))
                (when (= (% seed 3) 0) (thread-yield))
                (make-list 32 round)
                (when (= (% round 8) 0) (garbage-collect))
                (unless (= neovm-g3d-special owner)
                  (error "dynamic binding leaked")))
              (list (car only-here) (length (cadr only-here))
                    neovm-g3d-special)))
          (format "g3d-owner-%d" owner))
         threads)))
    (list (mapcar #'thread-join (nreverse threads)) neovm-g3d-special
          (thread-last-error t))))
"##,
        expect_test::expect![[
            r#""OK (((0 32 0) (1 32 1) (2 32 2) (3 32 3) (4 32 4) (5 32 5) (6 32 6) (7 32 7)) canonical nil)""#
        ]],
    );
}

#[test]
fn green_byte_compiled_parked_frames_survive_other_threads_gc() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (defvar neovm-g3d-go nil)
  (defvar neovm-g3d-parked 0)
  (defalias 'neovm-g3d-deep
    (byte-compile
     (lambda (n)
       (if (= n 0)
           (progn (setq neovm-g3d-parked (1+ neovm-g3d-parked))
                  (while (not neovm-g3d-go) (thread-yield))
                  nil)
         (let ((mine (make-list 50 n)))
           (cons (apply #'+ mine) (neovm-g3d-deep (1- n))))))))
  (let ((workers (mapcar (lambda (_) (make-thread (lambda () (neovm-g3d-deep 12))))
                         '(1 2 3))))
    (while (< neovm-g3d-parked 3) (thread-yield))
    (dotimes (_ 3)
      (dotimes (i 2000) (make-list 20 i))
      (garbage-collect))
    (setq neovm-g3d-go t)
    (mapcar #'thread-join workers)))
"##,
        expect_test::expect![[
            r#""OK ((600 550 500 450 400 350 300 250 200 150 100 50) (600 550 500 450 400 350 300 250 200 150 100 50) (600 550 500 450 400 350 300 250 200 150 100 50))""#
        ]],
    );
}

// Round 3w: GNU-global state is one value for every Lisp thread.

#[test]
fn green_a_process_made_by_one_thread_is_every_threads() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let ((proc (thread-join
             (make-thread (lambda () (make-pipe-process :name "g3w-pipe" :noquery t))))))
  (prog1 (list (process-name proc)
               (eq (get-process "g3w-pipe") proc)
               (and (memq proc (process-list)) t)
               (format "%S" proc)
               (process-live-p proc))
    (delete-process proc)))
"##,
        expect_test::expect![[r##""OK (\"g3w-pipe\" t t \"#<process g3w-pipe>\" (open listen connect stop))""##]],
    );
}

#[test]
fn green_registers_abbrevs_and_kmacros_set_by_a_thread_are_mains() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (thread-join
   (make-thread
    (lambda ()
      (set-register ?q "g3w-register")
      (define-abbrev-table 'g3w-abbrev-table '(("g3wab" "g3w-expansion")))
      (setq last-kbd-macro [?a ?b]))))
  (list (get-register ?q)
        (abbrev-expansion "g3wab" g3w-abbrev-table)
        last-kbd-macro))
"##,
        expect_test::expect![[r#""OK (\"g3w-register\" \"g3w-expansion\" [97 98])""#]],
    );
}

#[test]
fn green_a_minor_mode_and_a_face_defined_by_a_thread_are_mains() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (thread-join
   (make-thread
    (lambda ()
      (define-minor-mode g3w-minor-mode "A 3w test mode." :lighter " G3W")
      (defface g3w-face '((t :weight bold)) "A 3w test face." :group 'basic-faces)
      (g3w-minor-mode 1))))
  (list (fboundp 'g3w-minor-mode)
        (and (memq 'g3w-minor-mode minor-mode-list) t)
        g3w-minor-mode
        (facep 'g3w-face)
        (face-attribute 'g3w-face :weight)
        (and (memq 'g3w-face (face-list)) t)))
"##,
        expect_test::expect![[r#""OK (t t t [face unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified unspecified] bold t)""#]],
    );
}

#[test]
fn green_the_echo_area_and_the_time_zone_are_one_for_every_thread() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (thread-join
   (make-thread
    (lambda ()
      (message "g3w-message")
      (set-time-zone-rule "XXX-5"))))
  (prog1 (list (current-message) (current-time-zone))
    (set-time-zone-rule nil)))
"##,
        expect_test::expect![[r#""OK (nil (18000 \"XXX\"))""#]],
    );
}

#[test]
fn green_memory_use_counts_include_every_threads_consing() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let ((before (car (memory-use-counts))))
  (thread-join (make-thread (lambda () (make-list 10000 'x) nil)))
  (>= (- (car (memory-use-counts)) before) 10000))
"##,
        expect_test::expect![[r#""OK t""#]],
    );
}

#[test]
fn green_a_profiler_started_by_a_thread_runs_for_every_thread() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(progn
  (thread-join (make-thread (lambda () (profiler-memory-start))))
  (prog1 (list (profiler-memory-running-p)
               (thread-join (make-thread (lambda () (profiler-memory-running-p)))))
    (profiler-memory-stop)
    (thread-join (make-thread (lambda () (profiler-memory-running-p))))))
"##,
        expect_test::expect![[r#""OK (t t)""#]],
    );
}

#[test]
fn green_after_change_calls_a_thread_deferred_run_from_main() {
    return_if_neovm_enable_oracle_proptest_not_set!();
    assert_green_oracle(
        r##"
(let ((buffer (generate-new-buffer "g3w-combine"))
      (log nil))
  (unwind-protect
      (progn
        (with-current-buffer buffer
          (insert "0123456789")
          (setq-local after-change-functions
                      (list (lambda (beg end len) (push (list beg end len) log)))))
        (setq combine-after-change-calls t)
        (thread-join
         (make-thread
          (lambda ()
            (with-current-buffer buffer
              (goto-char 3)
              (insert "ab")
              (goto-char 9)
              (insert "cd")))))
        (setq combine-after-change-calls nil)
        (with-current-buffer buffer
          (let ((before (copy-sequence log)))
            (combine-after-change-execute)
            (list before log (buffer-string)))))
    (setq combine-after-change-calls nil)
    (kill-buffer buffer)))
"##,
        expect_test::expect![[r#""OK (nil ((3 11 4)) \"01ab2345cd6789\")""#]],
    );
}
