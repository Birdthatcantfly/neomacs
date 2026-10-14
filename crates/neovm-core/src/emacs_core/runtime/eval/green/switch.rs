//! Thread switches (plan-3d §3): a Lisp thread gives the Baton up at a
//! switch point and runs again when it comes back.
//!
//! Parking publishes the owner's private roots into the World (§4): a
//! collection run by another owner traces them there, and never reads the
//! parked Context, which stays exclusively its own thread's.

use super::super::world::Baton;
use super::super::{Context, Flow, Value};
use super::scheduler::{Control, Handoff, Lifecycle, ThreadKey, Wait};
use crate::emacs_core::error::signal;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// The longest a thread waiting in `thread_select` leaves its own input
/// unwatched while other threads run.
const SELECT_QUANTUM: Duration = Duration::from_millis(10);

/// A Lisp thread's place in its Green runtime, kept in its owner slab.
pub(crate) struct GreenSeat {
    pub(crate) key: ThreadKey,
    pub(super) control: Arc<Control>,
    receiver: Option<mpsc::Receiver<Baton>>,
    /// The World's Baton, while this thread runs.
    baton: Option<Baton>,
}

impl std::fmt::Debug for GreenSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GreenSeat")
            .field("key", &self.key)
            .field("running", &self.baton.is_some())
            .finish_non_exhaustive()
    }
}

/// The root groups of `Context::trace_roots` that name World storage: the
/// running owner's own trace covers them, so a parked owner's record leaves
/// them out (else every record would copy every other one).
const WORLD_ROOT_GROUPS: &[&str] = &[
    "owned_native",
    "owner_bindings",
    "treesit",
    "shared_roots",
    "ccl_registry",
    "charset_registry",
    "terminal_registry",
    "font_registry",
    "hash_table_test_registry",
    "file_notify_registry",
    "dynamic_module_registry",
    "obarray",
    "autoloads",
    "buffers",
    "frames",
    "coding_systems",
];

/// Why a thread may not give the Baton up here.
fn switch_refusal(ctx: &Context) -> Option<&'static str> {
    if ctx.unwind_cleanup_depth != 0 {
        Some("Cannot switch threads inside an unwind cleanup")
    } else if ctx.gc_inhibit_depth != 0 {
        Some("Cannot switch threads while garbage collection is inhibited")
    } else if super::super::super::dynamic_module::module_call_active() {
        Some("Cannot switch threads inside a module call")
    } else {
        None
    }
}

impl Context {
    /// This owner's Green seat, if it runs in a Green runtime.
    pub(crate) fn green_seat(&mut self) -> Option<&mut GreenSeat> {
        self.owner.slab_mut()?.green_seat_mut()
    }

    /// The running thread's key, if this owner runs in a Green runtime.
    pub(crate) fn green_key(&mut self) -> Option<ThreadKey> {
        self.green_seat().map(|seat| seat.key)
    }

    /// Seat this owner, which holds BATON, in the runtime CONTROL as KEY.
    pub(super) fn take_green_seat(
        &mut self,
        key: ThreadKey,
        control: Arc<Control>,
        receiver: mpsc::Receiver<Baton>,
        baton: Baton,
    ) {
        self.ensure_owner_slab()
            .install_green_seat(Box::new(GreenSeat {
                key,
                control,
                receiver: Some(receiver),
                baton: Some(baton),
            }));
    }

    /// Copy every root only this owner holds: its private execution state
    /// and its thread-local roots, without the World's storage.
    fn collect_parked_roots(&mut self) -> Vec<Value> {
        let mut roots = Vec::new();
        let world_group = std::cell::Cell::new(false);
        self.trace_roots(
            &mut |name| world_group.set(WORLD_ROOT_GROUPS.contains(&name)),
            &mut |value| {
                if !world_group.get() {
                    roots.push(value);
                }
            },
        );
        let mut thread_local = Vec::new();
        let mut stats = Vec::new();
        super::super::collect_thread_local_gc_roots(
            &mut thread_local,
            self.tagged_heap.identity(),
            self.tagged_heap.gc_collections(),
            crate::tagged::gc::CacheRootScan::Collection,
            &mut stats,
        );
        roots.extend(thread_local.into_iter().map(|(value, _)| value));
        roots
    }

    /// Give the Baton up, waiting for WAIT, and run again when it comes
    /// back (GNU's release and re-acquisition of the global lock). A no-op
    /// outside a Green runtime. Signals when switching is refused here, when
    /// no thread can run and this one waits for something only a running
    /// thread could give (GNU hangs there; neomacs reports the deadlock), or
    /// when the process is exiting. A pending `thread-signal` stays pending:
    /// [`Context::green_wait`] raises it.
    pub(crate) fn green_park(&mut self, wait: Wait) -> Result<(), Flow> {
        let Some(seat) = self.green_seat() else {
            return Ok(());
        };
        let key = seat.key;
        let control = Arc::clone(&seat.control);
        if let Some(refusal) = switch_refusal(self) {
            return Err(signal("error", vec![Value::string(refusal)]));
        }
        let seat = self.green_seat().expect("seated above");
        let baton = seat
            .baton
            .take()
            .expect("the running thread holds the Baton");
        let receiver = seat.receiver.take().expect("a seat keeps its channel");
        // The thread record keeps what another thread may read or change
        // while this one is parked: its current buffer and its errors.
        let current = self.buffers.current_buffer_id();
        self.threads.set_thread_current_buffer(key.id(), current);
        self.threads.restore_deferred_thread_error(key.id());
        let parked = self.park_owner();
        self.exchange_world_threads(&baton);
        let roots = self.collect_parked_roots();
        self.publish_parked_roots(&baton, key, roots);
        let outcome = control.transfer(key, wait, baton, &receiver);
        let (baton, deadlocked) = match outcome {
            Ok(baton) => (baton, false),
            Err(Handoff::NoRunnable(baton)) => (baton, true),
        };
        self.retire_parked_roots(&baton, key);
        self.exchange_world_threads(&baton);
        self.threads.enter_thread(key.id());
        let exit = baton
            .green_runtime()
            .as_ref()
            .and_then(|runtime| runtime.exit_for(key, &control));
        let seat = self.green_seat().expect("seated above");
        seat.receiver = Some(receiver);
        seat.baton = Some(baton);
        // GNU `post_acquire_global_lock` sets the thread's buffer again: a
        // thread that killed it moved this one to another, which the
        // owner's mirrors must read from as they come back.
        let moved = self
            .threads
            .thread_current_buffer(key.id())
            .filter(|buffer| current != Some(*buffer) && self.buffers.get(*buffer).is_some());
        if let Some(buffer) = moved {
            let _ = self.buffers.switch_current_unrecorded(buffer);
        }
        self.resume_owner(parked);
        if moved.is_some() {
            let _ = self.sync_current_buffer_runtime_state();
        }
        // Another thread may have edited strings in place or changed the
        // syntax tables these caches answer from.
        crate::emacs_core::string_pos_cache::reset_string_pos_cache();
        crate::emacs_core::regex::reset_regex_thread_locals();
        if let Some(request) = exit {
            // The process exits: like GNU's `exit`, no cleanup form of this
            // thread runs on the way out.
            self.shutdown_request = Some(request);
            self.command_loop.running = false;
            return Err(Flow::shutdown(request));
        }
        if deadlocked {
            return Err(signal(
                "error",
                vec![Value::string(
                    "All threads are blocked: this one would wait forever \
                     (GNU Emacs hangs here; neomacs signals)",
                )],
            ));
        }
        Ok(())
    }

    /// [`Context::green_park`], then raise an error another thread signalled
    /// into this one (GNU `post_acquire_global_lock`).
    pub(crate) fn green_wait(&mut self, wait: Wait) -> Result<(), Flow> {
        let Some(key) = self.green_key() else {
            return Ok(());
        };
        self.green_park(wait)?;
        self.green_raise_pending_signal(key)
    }

    /// Raise the error another thread signalled into KEY, if any.
    pub(super) fn green_raise_pending_signal(&mut self, key: ThreadKey) -> Result<(), Flow> {
        match self.threads.take_pending_thread_signal(key.id()) {
            Some(flow) => Err(flow),
            None => Ok(()),
        }
    }

    /// GNU `thread_select`: a wait of WAIT_TIME for input or a deadline lets
    /// the other Lisp threads run. Returns true when this thread gave the
    /// Baton up instead of blocking, for at most [`SELECT_QUANTUM`] of the
    /// wait (it waits for its own input only while it runs); false when it
    /// should block itself: no other thread could run meanwhile, or
    /// switching is refused here.
    pub(crate) fn green_select_switch(&mut self, wait_time: Duration) -> Result<bool, Flow> {
        let Some(seat) = self.green_seat() else {
            return Ok(false);
        };
        let key = seat.key;
        let control = Arc::clone(&seat.control);
        if wait_time.is_zero() || switch_refusal(self).is_some() || !control.others_could_run(key) {
            return Ok(false);
        }
        let deadline = Instant::now() + wait_time.min(SELECT_QUANTUM);
        self.green_wait(Wait::Until(deadline))?;
        Ok(true)
    }

    /// Main is ending (its Context drops): every other thread ends without
    /// running more Lisp, as GNU's process exit abandons them, and main
    /// joins their OS threads.
    #[cold]
    #[inline(never)]
    pub(crate) fn green_teardown(&mut self) {
        let Some(seat) = self.green_seat() else {
            return;
        };
        if seat.key != ThreadKey::Main {
            return;
        }
        let control = Arc::clone(&seat.control);
        control.close();
        while control.others_could_run(ThreadKey::Main) {
            // Each hand-off ends a thread; main's own exit flow, if any, has
            // nowhere left to go.
            let _ = self.green_park(Wait::Runnable);
        }
        for (_, os_thread) in control.take_join_handles() {
            let _ = os_thread.join();
        }
    }

    /// Move the World's `ThreadManager` between this Context and the
    /// runtime's slot (`GreenRuntime::threads`): to the slot at a park, back
    /// at the resume.
    fn exchange_world_threads(&mut self, baton: &Baton) {
        if let Some(runtime) = baton.green_runtime().as_ref() {
            std::mem::swap(&mut self.threads, &mut runtime.threads.borrow_mut());
        }
    }

    /// Keep ROOTS traced while KEY is parked.
    fn publish_parked_roots(&self, baton: &Baton, key: ThreadKey, roots: Vec<Value>) {
        if let Some(runtime) = baton.green_runtime().as_ref() {
            runtime.parked.borrow_mut().insert(key, roots);
        }
    }

    /// KEY runs again: its roots are its own Context's once more.
    fn retire_parked_roots(&self, baton: &Baton, key: ThreadKey) {
        if let Some(runtime) = baton.green_runtime().as_ref() {
            runtime.parked.borrow_mut().remove(&key);
        }
    }

    /// GNU `make-thread` (ARGS: FUNCTION, NAME, BUFFER-DISPOSITION) in a
    /// Green World: a Lisp thread on its own OS thread, starting in this
    /// thread's current buffer and running in its turn. This thread keeps
    /// running (GNU does not yield here). Starts the Green runtime on first
    /// use. Returns the thread object.
    #[cold]
    #[inline(never)]
    pub(crate) fn green_spawn(&mut self, args: &[Value]) -> Result<Value, Flow> {
        if self.green_seat().is_none() {
            self.start_green_threads()
                .map_err(|error| signal("error", vec![Value::string(error.to_string())]))?;
        }
        let current = self.buffers.current_buffer_id();
        let (id, function) =
            crate::emacs_core::threads::prepare_make_thread(&mut self.threads, args)?;
        self.threads.set_thread_current_buffer(id, current);
        let handle = self.threads.thread_handle(id).unwrap_or(Value::NIL);
        let key = ThreadKey::from_id(id);
        let control = Arc::clone(&self.green_seat().expect("seated").control);
        let receiver = control.register_worker(key);
        self.with_green_runtime(|runtime| {
            runtime.pending_starts.borrow_mut().insert(key, function);
        });
        let worker_control = Arc::clone(&control);
        let os_thread = std::thread::Builder::new()
            .name(format!("neovm-lisp-{id}"))
            .spawn(move || run_worker(key, receiver, worker_control))
            .expect("spawning a Lisp thread's OS thread");
        control.note_join_handle(key, os_thread);
        Ok(handle)
    }

    /// Start the Green runtime and seat this owner as its main thread.
    #[cold]
    #[inline(never)]
    pub(crate) fn start_green_threads(&mut self) -> Result<(), super::GreenStartError> {
        let baton = self.start_green_runtime()?;
        let control = self.with_green_runtime(|runtime| Arc::clone(&runtime.control));
        let receiver = control.register_main();
        self.take_green_seat(ThreadKey::Main, control, receiver, baton);
        Ok(())
    }

    /// Run OP on this World's Green runtime (which has started).
    pub(super) fn with_green_runtime<R>(&self, op: impl FnOnce(&super::GreenRuntime) -> R) -> R {
        let runtime = self.green_runtime_ref();
        op(runtime.as_ref().expect("the Green runtime has started"))
    }
}

/// A Lisp thread's OS thread: wait for the Baton, build the thread's
/// owner, run its function, record the outcome and hand the Baton on.
fn run_worker(key: ThreadKey, receiver: mpsc::Receiver<Baton>, control: Arc<Control>) {
    // A panic here would drop the Baton with this thread's owner, and every
    // parked thread would wait for it forever: end the process instead.
    let _abort_on_panic = AbortOnPanic;
    let Ok(baton) = receiver.recv() else {
        // The runtime went away before this thread ever ran.
        return;
    };
    baton.enter();
    if control.lifecycle() == Lifecycle::Closing {
        // Main ended before this thread ever ran.
        if let Some(runtime) = baton.green_runtime().as_ref() {
            runtime.threads.borrow_mut().mark_thread_exited(key.id());
            runtime.pending_starts.borrow_mut().remove(&key);
        }
        control.finish(key, baton);
        return;
    }
    let mut owner = Context::new_green_owner(&baton);
    owner.exchange_world_threads(&baton);
    if let Some(buffer) = owner.threads.thread_current_buffer(key.id()) {
        owner.restore_current_buffer_if_live(buffer);
    }
    owner.threads.enter_thread(key.id());
    owner.threads.start_thread(key.id());
    // GNU raises an error signalled into a thread that has not run yet at
    // its first switch, once it has handlers.
    owner.threads.defer_thread_error(key.id());
    let function = baton
        .green_runtime()
        .as_ref()
        .and_then(|runtime| runtime.pending_starts.borrow_mut().remove(&key));
    owner.take_green_seat(key, Arc::clone(&control), receiver, baton);
    let outcome = match function {
        Some(function) => {
            owner.push_vm_root_frame();
            owner.push_vm_frame_root(function);
            let outcome = owner.apply(function, Vec::new());
            owner.pop_vm_root_frame();
            outcome
        }
        None => Ok(Value::NIL),
    };
    let exit = outcome.as_ref().err().and_then(Flow::shutdown_request);
    // A kill-emacs is the process's, not this thread's: it is not recorded.
    let _ = crate::emacs_core::threads::finish_make_thread_result(
        &mut owner.threads,
        key.id(),
        outcome,
    );
    owner.threads.restore_deferred_thread_error(key.id());
    owner.threads.mark_thread_exited(key.id());
    let seat = owner.green_seat().expect("seated above");
    let baton = seat
        .baton
        .take()
        .expect("a finishing thread holds the Baton");
    owner.exchange_world_threads(&baton);
    if let Some(request) = exit
        && control.lifecycle() == Lifecycle::Running
        && let Some(runtime) = baton.green_runtime().as_ref()
    {
        // GNU's `kill-emacs` exits the process from any thread: main runs
        // next, only to leave.
        runtime.exit.set(Some(request));
        control.prioritize(ThreadKey::Main);
    }
    owner.retire_green_owner();
    control.finish(key, baton);
}

/// Aborts the process when dropped by a panic.
struct AbortOnPanic;

impl Drop for AbortOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            std::process::abort();
        }
    }
}
