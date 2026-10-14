//! Phase 0 step 2, round 3d: Lisp threads switching on the Baton.

use crate::emacs_core::eval::{Context, ThreadingMode};
use crate::emacs_core::print::print_value;
use crate::emacs_core::value::Value;

fn eval(ev: &mut Context, src: &str) -> String {
    match ev.eval_str(src) {
        Ok(value) => print_value(&value),
        Err(crate::emacs_core::error::EvalError::Signal { symbol, data, .. }) => format!(
            "ERR {} {}",
            crate::emacs_core::intern::resolve_sym(symbol),
            print_value(&Value::list(data))
        ),
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

#[test]
fn a_mutex_serializes_threads_across_a_yield() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgm-m (make-mutex \"tgm\") tgm-log nil)
               (let ((a (make-thread (lambda ()
                                       (mutex-lock tgm-m)
                                       (setq tgm-log (cons 'a1 tgm-log))
                                       (thread-yield)
                                       (setq tgm-log (cons (eq (thread--blocker tgm-b) tgm-m) tgm-log))
                                       (mutex-unlock tgm-m)))))
                 (setq tgm-b (make-thread (lambda ()
                                            (mutex-lock tgm-m)
                                            (setq tgm-log (cons 'b1 tgm-log))
                                            (mutex-unlock tgm-m))))
                 (thread-join a)
                 (thread-join tgm-b)
                 (list (nreverse tgm-log) (thread--blocker tgm-b))))"
        ),
        "((a1 t b1) nil)"
    );
}

#[test]
fn a_mutex_is_recursive_and_only_its_owner_unlocks_it() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgr-m (make-mutex))
               (mutex-lock tgr-m)
               (mutex-lock tgr-m)
               (list (thread-join
                      (make-thread
                       (lambda ()
                         (condition-case e (mutex-unlock tgr-m) (error (car (cdr e)))))))
                     (mutex-unlock tgr-m)
                     (mutex-unlock tgr-m)
                     (condition-case e (mutex-unlock tgr-m) (error 'not-held))))"
        ),
        "(\"Cannot unlock mutex owned by another thread\" nil nil not-held)"
    );
}

#[test]
fn condition_wait_releases_the_mutex_until_notified() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgc-m (make-mutex) tgc-c (make-condition-variable tgc-m)
                     tgc-q nil tgc-got nil)
               (let ((consumer (make-thread
                                (lambda ()
                                  (mutex-lock tgc-m)
                                  (while (null tgc-q) (condition-wait tgc-c))
                                  (setq tgc-got (list (car tgc-q)
                                                      (thread--blocker (current-thread))))
                                  (mutex-unlock tgc-m)))))
                 (thread-yield)
                 (list (eq (thread--blocker consumer) tgc-c)
                       (progn (mutex-lock tgc-m)
                              (setq tgc-q (list 42))
                              (condition-notify tgc-c)
                              (mutex-unlock tgc-m)
                              (thread-join consumer)
                              tgc-got))))"
        ),
        "(t (42 nil))"
    );
}

#[test]
fn condition_notify_wakes_one_waiter_or_all() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgn-m (make-mutex) tgn-c (make-condition-variable tgn-m) tgn-woke 0)
               (let ((ths (mapcar (lambda (_)
                                    (make-thread
                                     (lambda ()
                                       (mutex-lock tgn-m)
                                       (condition-wait tgn-c)
                                       (setq tgn-woke (1+ tgn-woke))
                                       (mutex-unlock tgn-m))))
                                  '(1 2 3))))
                 (thread-yield)
                 (mutex-lock tgn-m) (condition-notify tgn-c) (mutex-unlock tgn-m)
                 (thread-yield) (thread-yield)
                 (let ((one tgn-woke))
                   (mutex-lock tgn-m) (condition-notify tgn-c t) (mutex-unlock tgn-m)
                   (mapc #'thread-join ths)
                   (list one tgn-woke))))"
        ),
        "(1 3)"
    );
}

#[test]
fn condition_wait_takes_the_mutex_back_with_its_count() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgk-m (make-mutex) tgk-c (make-condition-variable tgk-m))
               (let ((th (make-thread
                          (lambda ()
                            (mutex-lock tgk-m)
                            (mutex-lock tgk-m)
                            (condition-wait tgk-c)
                            (list (mutex-unlock tgk-m)
                                  (mutex-unlock tgk-m)
                                  (condition-case nil (mutex-unlock tgk-m)
                                    (error 'released)))))))
                 (thread-yield)
                 (mutex-lock tgk-m) (condition-notify tgk-c) (mutex-unlock tgk-m)
                 (thread-join th)))"
        ),
        "(nil nil released)"
    );
}

#[test]
fn thread_signal_ends_a_wait_for_a_mutex_a_condition_or_a_join() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgw-m (make-mutex) tgw-n (make-mutex)
                     tgw-c (make-condition-variable tgw-n))
               (mutex-lock tgw-m)
               (let* ((locker (setq tgw-locker (make-thread
                               (lambda ()
                                 (condition-case e (progn (mutex-lock tgw-m) 'locked)
                                   (error (list 'lock e)))))))
                      (waiter (make-thread
                               (lambda ()
                                 (mutex-lock tgw-n)
                                 (condition-case e (progn (condition-wait tgw-c) 'notified)
                                   (error (list 'wait e (mutex-unlock tgw-n)))))))
                      (joiner (make-thread
                               (lambda ()
                                 (condition-case e (progn (thread-join tgw-locker) 'joined)
                                   (error (list 'join e)))))))
                 (thread-yield)
                 (thread-signal locker 'error '(\"l\"))
                 (thread-signal waiter 'error '(\"w\"))
                 (thread-signal joiner 'error '(\"j\"))
                 (thread-yield)
                 (mutex-unlock tgw-m)
                 (mapcar #'thread-join (list locker waiter joiner))))"
        ),
        "((lock (error \"l\")) (wait (error \"w\") nil) (join (error \"j\")))"
    );
}

#[test]
fn a_signal_into_a_thread_that_has_not_run_waits_for_its_first_switch() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgd-log nil)
               (let ((th (make-thread
                          (lambda ()
                            (setq tgd-log (cons 'start tgd-log))
                            (condition-case nil (setq tgd-log (cons 'body tgd-log))
                              (error (setq tgd-log (cons 'early tgd-log))))
                            (condition-case e (thread-yield)
                              (error (setq tgd-log (cons (list 'at-yield e) tgd-log))))))))
                 (thread-signal th 'error '(\"x\"))
                 (thread-yield)
                 (thread-yield)
                 (thread-join th)
                 (nreverse tgd-log)))"
        ),
        "(start body (at-yield (error \"x\")))"
    );
}

#[test]
fn thread_join_re_raises_a_signal_pending_when_it_began() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgj-m (make-mutex))
               (mutex-lock tgj-m)
               (let ((th (make-thread (lambda ()
                                        (condition-case nil (mutex-lock tgj-m)
                                          (error 'caught))))))
                 (thread-yield)
                 (thread-signal th 'error '(\"y\"))
                 (prog1 (condition-case e (thread-join th) (error (list 'join e)))
                   (mutex-unlock tgj-m))))"
        ),
        "(join (error \"y\"))"
    );
}

#[test]
fn a_signal_to_the_main_thread_raises_nothing_in_batch() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(list (thread-join (make-thread (lambda ()
                                              (thread-signal main-thread 'error '(\"m\"))
                                              'sent)))
                   (thread-yield)
                   'main-ok)"
        ),
        "(sent nil main-ok)"
    );
}

#[test]
fn a_thread_is_dead_once_its_function_returns_joined_or_not() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(let ((th (make-thread (lambda () 1))))
               (list (thread-live-p th)
                     (length (all-threads))
                     (progn (thread-yield) (thread-live-p th))
                     (length (all-threads))
                     (thread-join th)))"
        ),
        "(t 2 nil 1 1)"
    );
}

#[test]
fn waits_let_other_threads_run() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgz-a 0 tgz-b 0)
               (make-thread (lambda () (setq tgz-a 1)))
               (sleep-for 0.02)
               (make-thread (lambda () (setq tgz-b 1)))
               (accept-process-output nil 0.02)
               (list tgz-a tgz-b))"
        ),
        "(1 1)"
    );
}

#[test]
fn sleeping_threads_sleep_at_the_same_time() {
    let mut ev = green();
    let started = std::time::Instant::now();
    assert_eq!(
        eval(
            &mut ev,
            "(let ((ths (mapcar (lambda (_) (make-thread (lambda () (sleep-for 0.2) 'woke)))
                                '(1 2 3 4))))
               (sleep-for 0.2)
               (mapcar #'thread-join ths))"
        ),
        "(woke woke woke woke)"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(600),
        "five 0.2 s sleeps took {elapsed:?}"
    );
}

#[test]
fn killing_a_threads_buffer_moves_it_and_signals_it() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgb-log nil)
               (let* ((b (setq tgb-b (get-buffer-create \"tgb-x\")))
                      (th (save-current-buffer
                            (set-buffer b)
                            (make-thread
                             (lambda ()
                               (condition-case e (progn (thread-yield) (setq tgb-log (cons 'resumed tgb-log)))
                                 (error (setq tgb-log (cons (list e (eq (current-buffer) tgb-b)
                                                                (buffer-live-p (current-buffer)))
                                                          tgb-log)))))))))
                 (thread-yield)
                 (setq tgb-log (cons (list 'kill (kill-buffer b) (buffer-live-p b)) tgb-log))
                 (setq tgb-log (cons (condition-case e (thread-join th) (error (list 'join e))) tgb-log))
                 (nreverse tgb-log)))"
        ),
        "((kill t nil) ((thread-buffer-killed) nil t) (join (thread-buffer-killed)))"
    );
}

#[test]
fn a_thread_buffer_disposition_of_silently_or_t() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgq-log nil)
               (let* ((s (get-buffer-create \"tgq-s\"))
                      (k (get-buffer-create \"tgq-k\"))
                      (quiet (save-current-buffer
                               (set-buffer s)
                               (make-thread (lambda ()
                                              (thread-yield)
                                              (setq tgq-log (cons (list 'quiet (buffer-live-p (current-buffer)))
                                                                tgq-log)))
                                            nil 'silently)))
                      (keeper (save-current-buffer
                                (set-buffer k)
                                (make-thread (lambda ()
                                               (thread-yield)
                                               (setq tgq-log (cons (list 'keeper (buffer-name)) tgq-log)))
                                             nil t))))
                 (thread-yield)
                 (setq tgq-log (cons (list (kill-buffer s) (kill-buffer k) (buffer-live-p k)) tgq-log))
                 (thread-join quiet)
                 (thread-join keeper)
                 (nreverse tgq-log)))"
        ),
        "((t nil t) (quiet t) (keeper \"tgq-k\"))"
    );
}

#[test]
fn the_last_thread_to_block_is_told_that_all_threads_are_blocked() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgl-m (make-mutex) tgl-c (make-condition-variable tgl-m))
               (thread-join (make-thread (lambda ()
                                           (mutex-lock tgl-m)
                                           (condition-case e (condition-wait tgl-c)
                                             (error (car (cdr e))))))))"
        ),
        "\"All threads are blocked: this one would wait forever (GNU Emacs hangs here; neomacs signals)\""
    );
}

#[test]
fn kill_emacs_in_a_thread_exits_from_main_without_cleanups() {
    let mut ev = green();
    eval(&mut ev, "(setq tgx-cleanup nil tgx-m (make-mutex))");
    let outcome = eval(
        &mut ev,
        "(progn
           (mutex-lock tgx-m)
           (make-thread (lambda ()
                          (unwind-protect (mutex-lock tgx-m)
                            (setq tgx-cleanup 'blocked-thread))))
           (unwind-protect
               (thread-join (make-thread (lambda ()
                                           (unwind-protect (kill-emacs 7)
                                             (setq tgx-cleanup 'killer)))))
             (setq tgx-cleanup 'main)))",
    );
    assert!(outcome.starts_with("ERR"), "{outcome}");
    assert_eq!(
        ev.shutdown_request().map(|request| request.exit_code),
        Some(7)
    );
    assert!(ev.visible_variable_value_or_nil("tgx-cleanup").is_nil());
    ev.green_teardown();
    assert!(ev.visible_variable_value_or_nil("tgx-cleanup").is_nil());
}

#[test]
fn main_ending_ends_every_thread_without_running_their_cleanups() {
    let mut ev = green();
    let started = std::time::Instant::now();
    eval(
        &mut ev,
        "(progn
           (setq tgt-m (make-mutex) tgt-ran nil)
           (mutex-lock tgt-m)
           (setq tgt-a (make-thread (lambda ()
                                      (unwind-protect (mutex-lock tgt-m)
                                        (setq tgt-ran 'a)))))
           (setq tgt-b (make-thread (lambda ()
                                      (unwind-protect (sleep-for 30)
                                        (setq tgt-ran 'b)))))
           (thread-yield)
           (setq tgt-c (make-thread (lambda () (setq tgt-ran 'c)))))",
    );
    ev.green_teardown();
    assert!(ev.visible_variable_value_or_nil("tgt-ran").is_nil());
    assert_eq!(
        eval(
            &mut ev,
            "(list (thread-live-p tgt-a) (thread-live-p tgt-b) (thread-live-p tgt-c))"
        ),
        "(nil nil nil)"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    drop(ev);
}

/// `(lambda () (list (make-list 100 'x) (tgv-next) (make-list 100 'y)))` as
/// byte-code: its first list lives only on the VM stack across the call.
const TGV_BYTECODE: &str = "(fset 'tgv-f
   (make-byte-code nil
                   (unibyte-string 192 193 194 195 34 196 32 193 194 197 34 35 135)
                   [list make-list 100 x tgv-next y]
                   6))";

#[test]
fn a_parked_threads_vm_frames_survive_other_threads_collections() {
    let mut ev = green();
    eval(&mut ev, TGV_BYTECODE);
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgv-depth 10)
               (fset 'tgv-next (lambda ()
                                 (if (> tgv-depth 0)
                                     (progn (setq tgv-depth (1- tgv-depth)) (tgv-f))
                                   (thread-yield)
                                   'bottom)))
               (fset 'tgv-count (lambda (r)
                                  (let ((n 0))
                                    (while (consp r)
                                      (let ((xs (car r)))
                                        (while xs
                                          (if (eq (car xs) 'x) (setq n (1+ n)))
                                          (setq xs (cdr xs))))
                                      (setq r (car (cdr r))))
                                    n)))
               (let ((th (make-thread (lambda () (tgv-count (tgv-f))))))
                 (thread-yield)
                 (let ((i 0))
                   (while (< i 3)
                     (let ((j 0)) (while (< j 3000) (make-list 20 j) (setq j (1+ j))))
                     (garbage-collect)
                     (setq i (1+ i))))
                 (thread-join th)))"
        ),
        "1100"
    );
}

#[test]
fn eight_threads_yield_and_collect_without_losing_values() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (fset 'tg8-run
                     (lambda (n)
                       (let ((mine nil) (k 0))
                         (while (< k 30)
                           (setq mine (cons (make-list 20 (+ (* n 1000) k)) mine))
                           (thread-yield)
                           (if (= (% k 10) 0) (garbage-collect))
                           (setq k (1+ k)))
                         (let ((sum 0))
                           (while mine
                             (let ((xs (car mine)))
                               (while xs (setq sum (+ sum (car xs)) xs (cdr xs))))
                             (setq mine (cdr mine)))
                           sum))))
               (let ((ths nil) (i 0))
                 (while (< i 8)
                   (setq ths (cons (make-thread (list 'lambda nil (list 'tg8-run i))) ths))
                   (setq i (1+ i)))
                 (mapcar #'thread-join (nreverse ths))))"
        ),
        "(8700 608700 1208700 1808700 2408700 3008700 3608700 4208700)"
    );
}

#[test]
fn every_thread_shares_the_standard_case_table() {
    let mut ev = green();
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgct-old (standard-case-table)
                     tgct-new (make-char-table 'case-table))
               (let ((early (make-thread (lambda ()
                                           (thread-yield)
                                           (eq (standard-case-table) tgct-new)))))
                 (thread-yield)
                 (set-standard-case-table tgct-new)
                 (prog1 (list (thread-join early)
                              (thread-join (make-thread
                                            (lambda () (eq (standard-case-table) tgct-new))))
                              (eq (standard-case-table) tgct-new))
                   (set-standard-case-table tgct-old))))"
        ),
        "(t t t)"
    );
}

#[test]
fn the_standard_case_table_is_one_table_after_admission() {
    let mut ev = green();
    ev.admit_threads().expect("admit");
    assert_eq!(
        eval(
            &mut ev,
            "(progn
               (setq tgsa-old (standard-case-table) tgsa-new (make-char-table 'case-table))
               (list (eq (standard-case-table) tgsa-old)
                     (eq (set-standard-case-table tgsa-new) tgsa-new)
                     (eq (standard-case-table) tgsa-new)
                     (progn (set-standard-case-table tgsa-old)
                            (list (upcase \"abc\") (eq (standard-case-table) tgsa-old)))))"
        ),
        "(t t t (\"ABC\" t))"
    );
}
