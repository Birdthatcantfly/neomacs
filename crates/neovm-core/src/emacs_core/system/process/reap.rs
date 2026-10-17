//! The single owner of `waitpid` in this port, and GNU's `p->alive` as a type
//! rather than a `bool`.
//!
//! # GNU has exactly one `waitpid`, and says why
//!
//! `grep -n 'waitpid' src/*.c` over the GNU tree returns **one** call in the
//! whole POSIX build of the editor: `src/sysdep.c:471`, inside
//! `get_child_status`, which is `static`.  Everything reaches it through two
//! wrappers -- `child_status_changed` (`WNOHANG`, :515) and
//! `wait_for_termination` (blocking, :500) -- and it opens with
//!
//! ```c
//!   /* Invoke waitpid only with a known process ID; do not invoke
//!      waitpid with a nonpositive argument.  Otherwise, Emacs might
//!      reap an unwanted process by mistake.  For example, invoking
//!      waitpid (-1, ...) can mess up glib by reaping glib's subprocesses,
//!      so that another thread running glib won't find them.  */
//!   eassert (child > 0);                             /* src/sysdep.c:456-462 */
//! ```
//!
//! The rule the choke point exists to keep is stated in full one file over,
//! above `deleted_pid_list` (src/process.c:1080-1088):
//!
//! ```text
//!    The main Emacs thread invokes waitpid only on child processes that
//!    it creates and that have not been reaped.  This avoid races on
//!    platforms such as GTK, where other threads create their own
//!    subprocesses which the main thread should not reap.  For example,
//!    if the main thread attempted to reap an already-reaped child, it
//!    might inadvertently reap a GTK-created process that happened to
//!    have the same process ID.
//! ```
//!
//! Two clauses, and they are different obligations.  *"that it creates"* is
//! about the population, and GNU keeps it with three enumerated registries
//! (`Vprocess_alist`, `deleted_pid_list`, and `Fcall_process`'s local `PID`).
//! ***"and that have not been reaped"* is about time**, and GNU keeps it with
//! one bit -- `p->alive`, cleared at the instant the reap happens
//! (`handle_child_signal`, src/process.c:7754).
//!
//! # Where that bit is READ, which is the half this port got wrong
//!
//! GNU never re-asks the OS whether a child is still alive.
//! `record_kill_process` (src/callproc.c:196-211) is the whole of GNU's
//! "terminate this child" operation:
//!
//! ```c
//!   /* If P is reapable, record it as a deleted process and kill it.
//!      Do this in a critical section.  */
//!   block_child_signal (&oldset);
//!   if (p->alive)
//!     {
//!       record_deleted_pid (p->pid, tempfile);
//!       p->alive = 0;
//!       kill (- p->pid, SIGKILL);
//!     }
//!   unblock_child_signal (&oldset);
//! ```
//!
//! -- a recorded bit, tested inside a critical section so the SIGCHLD handler
//! cannot clear it between the test and the `kill`.  `Fdelete_process` reaches
//! it as `if (p->alive) record_kill_process (p, Qnil);`
//! (src/process.c:1134-1135).
//!
//! And it is not only the delete path.  `process.c` contains exactly **two**
//! `kill` calls, and the one every signal subr reaches -- `kill-process`,
//! `interrupt-process`, `stop-process`, `quit-process`, `continue-process` --
//! is gated the same way, with the hazard spelled out
//! (`process_send_signal`, :7199-7205):
//!
//! ```c
//!   /* Do not kill an already-reaped process, as that could kill an
//!      innocent bystander that happens to have the same process ID.  */
//!   sigset_t oldset;
//!   block_child_signal (&oldset);
//!   if (p->alive)
//!     kill (pid, signo);
//!   unblock_child_signal (&oldset);
//! ```
//!
//! The other (`Finternal_default_signal_process`, :7399) is GNU's documented
//! *"PROCESS may also be a number ... the process need not be a child of this
//! Emacs"* path and is deliberately ungated -- including when it takes the pid
//! from a process object at :7377.  This port routes that subr through
//! `deliver_process_signal` too, so it is gated here where GNU's is not; the
//! answers coincide (`kill` on a reaped pid fails `ESRCH`) except when the pid
//! has been recycled, which is the case :7199's comment is about.
//!
//! This port had no such bit.  `LiveProcessIo::terminate_and_reap_children`
//! spelled the same guard as a fresh probe:
//!
//! ```ignore
//!   if let Some(child) = self.child.as_mut()
//!       && !matches!(child.try_wait(), Ok(Some(_)))
//!   {
//!       sys::send_signal_to_group(child.id() as i64, signal_kill_number());
//!       let _ = child.wait();
//!   }
//! ```
//!
//! and a probe cannot answer that question, for two reasons that compound.
//! `Child::try_wait` **is itself a `waitpid`**, so on a child this port had
//! already reaped through `sys::poll_child_status` it is the second reap GNU's
//! comment forbids; and its answer there is `Err(ECHILD)` -- *"nobody has that
//! child"* -- which `!matches!(.., Ok(Some(_)))` reads as **"still running"**
//! and follows with `kill(-pid, SIGKILL)` on a pid the kernel has handed back.
//!
//! # The type
//!
//! [`ChildOwnership`] is `p->alive` with the pid attached to the live arm:
//!
//! * [`ChildOwnership::Unreaped`] holds a [`ReapableChild`], whose fields are
//!   private to this module. All process-table reaps and transfers go through
//!   this owner; the child handle cannot be probed independently.
//! * [`ChildOwnership::Reaped`] carries **no pid**, so
//!   `kill` on a reaped pid is not a case that is checked for: it is a
//!   sentence with no subject.
//! * [`ChildOwnership::Deferred`] also carries no pid: asynchronous deletion
//!   transferred the sole unreaped handle into the deleted-child registry.
//! * every transition to `Reaped` happens *inside* the operation that
//!   performed the reap, so the bit cannot be forgotten the way a hand-set
//!   `bool` can.
//!
//! Each process-table owner belongs to its mutator. The deferred registry
//! contains native handles only and serializes transfers and nonblocking
//! sweeps with a mutex, allowing multiple mutators to collect safely. Signal
//! handlers never access either owner; they only wake the existing wait path.
//!
//! The honest limit, stated the way `os_signal.rs` states its own: Rust does
//! not stop a future author from writing a fresh `libc::waitpid` elsewhere.
//! What this buys is that the *existing* reaping sites in the process table
//! collapse to one owner and that the owner cannot be reached without moving
//! through the state machine.

use super::{Value, process_status_exit_value};
use crate::emacs_core::callproc::SpawnedChild;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// GNU's deleted_pid_list (process.c:7700-7715). Each entry owns the only
/// handle permitted to reap its child, and contains no Lisp state. Native exit
/// watchers collect independently of the evaluator; failed native sources fall
/// back to existing child-status safe points. The mutex serializes
/// transfers and nonblocking probes across mutators and watchers. The acquire/release flag
/// publishes queue membership. Ordinary waits inspect only the fallback flag,
/// so native-managed deleted children add no locks or syscalls to Lisp polls.
static DEFERRED_CHILDREN: Mutex<Vec<DeferredChild>> = Mutex::new(Vec::new());
static HAS_DEFERRED_CHILDREN: AtomicBool = AtomicBool::new(false);
static HAS_SAFE_POINT_FALLBACK: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeferredReapDriver {
    NativeExit,
    WaitSafePointFallback,
}

#[derive(Clone, Copy, Debug)]
enum DeferredReapSweep {
    NativeExit,
    WaitSafePoint,
}

/// Native ownership plus a completion identity that cannot be confused with
/// a later child reusing the same pid. Registry and watcher share only this
/// atomic completion flag, never the child handle or Lisp state.
struct DeferredChild {
    owner: ReapableChild,
    completed: Arc<AtomicBool>,
    driver: DeferredReapDriver,
}

#[cfg(test)]
static DEFERRED_WATCHERS: Mutex<Option<Vec<std::thread::JoinHandle<()>>>> = Mutex::new(None);

/// Explicit, process-wide test opt-in to retaining native watcher handles.
/// Ordinary tests detach just like production. One scope owns retention across
/// all mutators in its test process; it must outlive the process I/O being
/// dropped. The guard stays on its creating test thread and joins on unwind.
#[cfg(test)]
#[must_use]
#[derive(Debug)]
pub(super) struct DeferredWatcherTestScope {
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
static_assertions::assert_not_impl_any!(DeferredWatcherTestScope: Send, Sync);

#[cfg(test)]
impl DeferredWatcherTestScope {
    pub(super) fn new() -> Self {
        let mut watchers = DEFERRED_WATCHERS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(
            watchers.is_none(),
            "native watcher test scopes cannot overlap"
        );
        *watchers = Some(Vec::new());
        Self {
            _thread_bound: std::marker::PhantomData,
        }
    }

    /// Join native exit watchers explicitly, after the test has closed its I/O.
    /// This does not drive a child-status sweep or reap on the test's behalf.
    pub(super) fn finish(self) {
        let handles = DEFERRED_WATCHERS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        // Join every handle before propagating any watcher panic. Drop also
        // joins handles registered while these joins were in progress.
        let panicked = handles
            .into_iter()
            .filter_map(|handle| handle.join().err())
            .count();
        assert_eq!(panicked, 0, "native deleted-child watcher panicked");
    }
}

#[cfg(test)]
impl Drop for DeferredWatcherTestScope {
    fn drop(&mut self) {
        let handles = DEFERRED_WATCHERS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .unwrap_or_default();
        for handle in handles {
            // Do not panic during unwinding. Explicit finish reports failures;
            // the worker's own panic has already been reported by Rust.
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
pub(super) fn deferred_child_is_owned_for_test(pid: u32) -> bool {
    DEFERRED_CHILDREN
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .any(|child| child.owner.pid == pid)
}

fn publish_deferred_child(child: ReapableChild, driver: DeferredReapDriver) -> Arc<AtomicBool> {
    let completed = Arc::new(AtomicBool::new(false));
    let mut children = DEFERRED_CHILDREN
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    children.push(DeferredChild {
        owner: child,
        completed: Arc::clone(&completed),
        driver,
    });
    HAS_DEFERRED_CHILDREN.store(true, Ordering::Release);
    if driver == DeferredReapDriver::WaitSafePointFallback {
        HAS_SAFE_POINT_FALLBACK.store(true, Ordering::Release);
    }
    completed
}

fn use_safe_point_fallback(completed: &Arc<AtomicBool>) {
    let mut children = DEFERRED_CHILDREN
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(child) = children
        .iter_mut()
        .find(|child| Arc::ptr_eq(&child.completed, completed))
    {
        child.driver = DeferredReapDriver::WaitSafePointFallback;
        HAS_SAFE_POINT_FALLBACK.store(true, Ordering::Release);
    }
}

fn start_deferred_child_watcher(
    event: std::io::Result<super::sys::ChildExitEvent>,
    completed: Arc<AtomicBool>,
) {
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            use_safe_point_fallback(&completed);
            tracing::warn!(%error, "deleted-child native exit registration failed; retaining deferred ownership");
            return;
        }
    };
    let watcher_completed = Arc::clone(&completed);
    match std::thread::Builder::new().name("neovm-child-reap".into()).spawn(move || {
        loop {
            // Registration may have raced an exit that is already waitable.
            // Sweep before sleeping, and after each actual native event.
            collect_deferred_children(DeferredReapSweep::NativeExit);
            if watcher_completed.load(Ordering::Acquire) { break; }
            if let Err(error) = event.wait() {
                use_safe_point_fallback(&watcher_completed);
                tracing::warn!(%error, "deleted-child native exit wait failed; retaining deferred ownership");
                break;
            }
        }
    }) {
        Ok(handle) => {
            #[cfg(test)]
            match DEFERRED_WATCHERS.lock().unwrap_or_else(|error| error.into_inner()).as_mut() {
                Some(watchers) => watchers.push(handle),
                None => drop(handle),
            }
            #[cfg(not(test))]
            drop(handle);
        }
        Err(error) => {
            use_safe_point_fallback(&completed);
            tracing::warn!(%error, "deleted-child watcher creation failed; retaining deferred ownership");
        }
    }
}

/// Only native-source failures need the ordinary wait's WNOHANG fallback.
/// Native-managed pending children never add a lock or syscall to zero-timeout
/// accept-process-output or input-pending-p polls.
pub(super) fn collect_deferred_child_status_changes() {
    collect_deferred_children(DeferredReapSweep::WaitSafePoint);
}

fn collect_deferred_children(sweep: DeferredReapSweep) {
    let pending = match sweep {
        DeferredReapSweep::NativeExit => &HAS_DEFERRED_CHILDREN,
        DeferredReapSweep::WaitSafePoint => &HAS_SAFE_POINT_FALLBACK,
    };
    if !pending.load(Ordering::Acquire) {
        return;
    }
    let mut children = DEFERRED_CHILDREN
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    children.retain_mut(|child| {
        match (sweep, child.driver) {
            (DeferredReapSweep::WaitSafePoint, DeferredReapDriver::NativeExit) => return true,
            (
                DeferredReapSweep::NativeExit,
                DeferredReapDriver::NativeExit | DeferredReapDriver::WaitSafePointFallback,
            )
            | (DeferredReapSweep::WaitSafePoint, DeferredReapDriver::WaitSafePointFallback) => {}
        }
        if child.owner.try_reap_deleted() {
            child.completed.store(true, Ordering::Release);
            false
        } else {
            true
        }
    });
    HAS_DEFERRED_CHILDREN.store(!children.is_empty(), Ordering::Release);
    HAS_SAFE_POINT_FALLBACK.store(
        children
            .iter()
            .any(|child| child.driver == DeferredReapDriver::WaitSafePointFallback),
        Ordering::Release,
    );
}

/// A child this port spawned and has **not** reaped: GNU's `p->alive == 1`,
/// carrying the pid that authorises `waitpid` and `kill` on it.
///
/// The fields are private and the only constructors are
/// [`ChildOwnership::of_pipe_child`] and [`ChildOwnership::of_pty_child`], so
/// the handles whose `try_wait` is a reap are not reachable from outside this
/// module.
pub(crate) struct ReapableChild {
    /// GNU's `p->pid` as a CAPABILITY.
    ///
    /// `Process::os_pid` keeps the same number for `Fprocess_id`, which GNU
    /// also keeps after the reap; this copy is the one that authorises a
    /// syscall, and it disappears with the reap.
    pid: u32,
    handle: ChildHandle,
}

/// Which kind of child handle a spawn left behind.
///
/// Both own a reap of their own -- `std::process::Child::try_wait` and
/// `portable_pty::Child::try_wait` are each a `waitpid` -- which is exactly
/// why neither may be reachable from anywhere else.
enum ChildHandle {
    /// `std::process::Child`: the pipe spawn, and the pty spawn whose
    /// `:stderr` split goes through `pre_exec`.
    Pipe(Box<SpawnedChild>),
    /// `portable_pty`'s child handle.
    Pty(Box<dyn portable_pty::Child + Send + Sync>),
}

static_assertions::assert_impl_all!(ReapableChild: Send);

/// What one probe of a child's status found.
///
/// The three arms are GNU's three outcomes at `handle_child_signal`
/// (src/process.c:7741-7757): no change; a change that leaves the child alive
/// (`WIFSTOPPED`/`WIFCONTINUED`, which is why GNU passes
/// `WUNTRACED | WCONTINUED`); and a change that clears `p->alive`
/// (`WIFSIGNALED (status) || WIFEXITED (status)`, :7750-7752).
#[derive(Debug)]
pub(crate) enum ChildStatusChange {
    /// `waitpid` returned 0: nothing to record.
    NoChange,
    /// A transition that leaves the pid this port's: `stop`, or `run` again
    /// after a continue.
    StillOurs(Value),
    /// A terminal transition.  The pid has gone back to the OS and this
    /// [`ChildOwnership`] is now [`ChildOwnership::Reaped`].
    Reaped(Value),
    /// `ECHILD`: **nobody** has that child, so there is a pid to give up and
    /// no status to record.
    ///
    /// Under [`ChildOwnership`] this can only mean an out-of-process reaper
    /// got there first -- GNU's `lib_child_handler` case
    /// (src/process.c:7654-7660, Glib).  GNU's own answer at that point is a
    /// negative `get_child_status` return and the comment *"Most likely,
    /// waitpid is buggy and the operating system lost track of the child
    /// somehow"* (src/sysdep.c:473-479), so no status is invented here
    /// either; what changes is only that the pid stops being spellable.  It
    /// used to be read as "no change", which left the delete path signalling
    /// a pid this port did not own.
    Gone,
}

/// GNU's `p->alive`, with the pid attached to the arm that may use it.
///
/// [`Default`] is [`Self::NotAChild`], which is what ledger 165's finding
/// needs: a pipe, network or serial connection has no pid at all, so
/// `get_child_status`'s `eassert (child > 0)` (src/sysdep.c:462) is a state
/// this enum cannot enter rather than a check inside a loop.
#[derive(Default)]
pub(crate) enum ChildOwnership {
    /// No child was ever spawned: `make-pipe-process`,
    /// `make-network-process`, `make-serial-process`, and the harness-only
    /// records with no OS backing.
    #[default]
    NotAChild,
    /// GNU's `p->alive == 1`.
    Unreaped(ReapableChild),
    /// GNU's `p->alive == 0`: the pid has gone back to the OS.
    ///
    /// It carries nothing, and that is the point -- there is no pid here for
    /// `kill(2)` or `waitpid(2)` to be given.  The terminal STATUS is not kept
    /// here either, because `Process::pending_status`/`Process::status` is
    /// where this port records it, exactly as GNU records it in
    /// `p->raw_status` rather than in `p->alive`.
    Reaped,
    /// The sole unreaped handle belongs to the deferred registry. This process
    /// cannot signal or wait for it again; a later safe point performs the reap.
    Deferred,
}

impl ChildOwnership {
    /// Take ownership of a freshly spawned `std::process::Child`.
    pub(crate) fn of_pipe_child(child: SpawnedChild) -> Self {
        Self::Unreaped(ReapableChild {
            pid: child.id(),
            handle: ChildHandle::Pipe(Box::new(child)),
        })
    }

    /// Take ownership of a freshly spawned `portable_pty` child.
    ///
    /// A child `portable_pty` cannot name a pid for is [`Self::NotAChild`],
    /// because a pidless child is exactly what `eassert (child > 0)` rejects
    /// one frame below GNU's own sweep.
    pub(crate) fn of_pty_child(child: Box<dyn portable_pty::Child + Send + Sync>) -> Self {
        match child.process_id() {
            Some(pid) => Self::Unreaped(ReapableChild {
                pid,
                handle: ChildHandle::Pty(child),
            }),
            None => Self::NotAChild,
        }
    }

    /// Whether a child was ever spawned -- live or already reaped.
    ///
    /// This is *not* `p->alive`: a reaped child is still a child, and the
    /// `Real`-process predicates that ask "does this record have OS backing?"
    /// mean this question rather than the liveness one.
    pub(crate) fn has_child(&self) -> bool {
        !matches!(self, Self::NotAChild)
    }

    /// GNU's `p->alive`: the pid, and only while this port still owns it.
    ///
    /// Every `kill(2)` aimed at one of this port's own children goes through
    /// here, so `record_kill_process`'s `if (p->alive)` is not a test anyone
    /// has to remember to write.
    pub(crate) fn pid_if_unreaped(&self) -> Option<u32> {
        match self {
            Self::Unreaped(child) => Some(child.pid),
            Self::NotAChild | Self::Reaped | Self::Deferred => None,
        }
    }

    /// The spawn stored a `portable_pty` handle rather than a
    /// `std::process::Child`.
    pub(crate) fn is_pty_handle(&self) -> bool {
        matches!(
            self,
            Self::Unreaped(ReapableChild {
                handle: ChildHandle::Pty(_),
                ..
            })
        )
    }

    /// The child's stdin while it exists.  Not a reap, and the only reason the
    /// handle is reachable at all.
    pub(crate) fn stdin_mut(&mut self) -> Option<&mut std::fs::File> {
        match self {
            Self::Unreaped(ReapableChild {
                handle: ChildHandle::Pipe(child),
                ..
            }) => child.stdin.as_mut(),
            _ => None,
        }
    }

    /// The child's stdin for the poller registrations, which need the
    /// descriptor and nothing else.
    pub(crate) fn stdin(&self) -> Option<&std::fs::File> {
        match self {
            Self::Unreaped(ReapableChild {
                handle: ChildHandle::Pipe(child),
                ..
            }) => child.stdin.as_ref(),
            _ => None,
        }
    }

    /// Whether this process has a pipe child at all -- the `else if let
    /// Some(child) = ...` this port's write path used to spell against the
    /// handle itself.
    pub(crate) fn has_pipe_child(&self) -> bool {
        matches!(
            self,
            Self::Unreaped(ReapableChild {
                handle: ChildHandle::Pipe(_),
                ..
            })
        )
    }

    /// GNU's `process-send-eof` for a pipe child: close stdin.  Answers
    /// whether there was a pipe child to close it on.
    pub(crate) fn close_stdin(&mut self) -> bool {
        match self {
            Self::Unreaped(ReapableChild {
                handle: ChildHandle::Pipe(child),
                ..
            }) => {
                drop(child.stdin.take());
                true
            }
            _ => false,
        }
    }

    /// GNU's `child_status_changed (p->pid, &status, WUNTRACED | WCONTINUED)`
    /// (src/process.c:7742), and the state transition its terminal answers
    /// cause.
    ///
    /// [`ChildStatusChange::NoChange`] for a process that is not, or is no
    /// longer, this port's to wait for -- which is GNU's `p->alive &&` at
    /// :7741 rather than an early `continue` inside the loop.
    pub(crate) fn poll_status(&mut self) -> ChildStatusChange {
        let Self::Unreaped(child) = self else {
            return ChildStatusChange::NoChange;
        };
        let change = child.probe();
        if matches!(
            change,
            ChildStatusChange::Reaped(_) | ChildStatusChange::Gone
        ) {
            *self = Self::Reaped;
        }
        change
    }

    /// GNU's `record_kill_process` (src/callproc.c:196-211): signal, then leave
    /// the sole child handle for a later nonblocking deleted-pid sweep.
    ///
    /// The `if (p->alive)` is the `let ... else`, and there is no other arm: a
    /// reaped child is neither signalled nor waited a second time.
    pub(crate) fn terminate_and_defer_reaping(&mut self, kill_signal: i32) {
        let Self::Unreaped(child) = self else {
            return;
        };
        // One probe, through the owner, so an already-exited child is settled
        // rather than signalled.  GNU reaches the same place from the other
        // side: its handler has already cleared `p->alive`.
        if child.try_reap_deleted() {
            *self = Self::Reaped;
            return;
        }
        child.kill_group_then_handle(kill_signal);
        self.defer_reaping();
    }

    /// GNU asynchronous delete records an unreaped pid instead of waiting
    /// (callproc.c:202-207). A Darwin child can still be draining its terminal
    /// during exit, so blocking here before closing the master can deadlock.
    pub(crate) fn defer_reaping(&mut self) {
        if let Some(child) = self.take_unreaped_for_transfer() {
            // Register while the pid still belongs exclusively to this owner.
            // Publishing first would allow another mutator to reap/recycle it.
            let event = super::sys::ChildExitEvent::open(child.pid);
            let driver = match &event {
                Ok(_) => DeferredReapDriver::NativeExit,
                Err(_) => DeferredReapDriver::WaitSafePointFallback,
            };
            let completed = publish_deferred_child(child, driver);
            // An early exit remains recorded by the already-open native source.
            start_deferred_child_watcher(event, completed);
        }
    }

    #[cfg(test)]
    fn defer_reaping_for_test(&mut self) {
        // Synthetic handles have no OS child. Exercise the same ownership
        // transfer without registering native events against their metadata.
        if let Some(child) = self.take_unreaped_for_transfer() {
            let _ = publish_deferred_child(child, DeferredReapDriver::WaitSafePointFallback);
        }
    }

    fn take_unreaped_for_transfer(&mut self) -> Option<ReapableChild> {
        match std::mem::replace(self, Self::Deferred) {
            Self::Unreaped(child) => Some(child),
            unchanged @ (Self::NotAChild | Self::Reaped | Self::Deferred) => {
                *self = unchanged;
                None
            }
        }
    }

    /// The handle's own `kill`, for the callers that fall back to it when the
    /// process group would not take the signal.
    pub(crate) fn kill_handle(&mut self) {
        let Self::Unreaped(child) = self else {
            return;
        };
        child.kill_handle();
    }
}

impl ReapableChild {
    /// One non-blocking probe, decoded into the port's status vocabulary.
    ///
    /// The dispatch is the one `poll_child_status_change` already had, kept
    /// exactly: a `portable_pty` child is probed through its own handle, and
    /// everything else through the raw `waitpid` -- which is what carries
    /// `WUNTRACED | WCONTINUED`, so the stop and continue transitions GNU asks
    /// for at :7742 stay visible.
    fn probe(&mut self) -> ChildStatusChange {
        match &mut self.handle {
            ChildHandle::Pty(child) => match child.try_wait() {
                Ok(Some(status)) => {
                    ChildStatusChange::Reaped(super::process_status_from_pty_exit(&status))
                }
                Ok(None) => ChildStatusChange::NoChange,
                Err(_) => ChildStatusChange::Reaped(process_status_exit_value(1)),
            },
            ChildHandle::Pipe(child) => Self::probe_pipe(self.pid, child),
        }
    }

    #[cfg(unix)]
    fn probe_pipe(pid: u32, _child: &mut SpawnedChild) -> ChildStatusChange {
        super::process_child_status_change_from_wait(super::sys::poll_child_status(pid))
    }

    #[cfg(not(unix))]
    fn probe_pipe(_pid: u32, child: &mut SpawnedChild) -> ChildStatusChange {
        match child.try_wait() {
            Ok(Some(status)) => ChildStatusChange::Reaped(super::process_status_from_exit(&status)),
            Ok(None) => ChildStatusChange::NoChange,
            Err(_) => ChildStatusChange::Reaped(process_status_exit_value(1)),
        }
    }

    /// Probe without allocating Lisp values: deleted children have no pending
    /// status or sentinel to publish. Errors other than ECHILD retain ownership
    /// for a later sweep. EINTR resumes immediately so a one-shot native exit
    /// event cannot be consumed without attempting the reap again.
    fn try_reap_deleted(&mut self) -> bool {
        loop {
            match &mut self.handle {
                #[cfg(unix)]
                ChildHandle::Pipe(_) => {
                    use super::sys::ChildWait;
                    match super::sys::poll_child_status(self.pid) {
                        ChildWait::Exited(_) | ChildWait::Signaled { .. } | ChildWait::NoChild => {
                            return true;
                        }
                        ChildWait::Running
                        | ChildWait::Stopped(_)
                        | ChildWait::Continued
                        | ChildWait::Undecoded => return false,
                        ChildWait::Error => {
                            if std::io::Error::last_os_error().kind()
                                != std::io::ErrorKind::Interrupted
                            {
                                return false;
                            }
                        }
                    }
                }
                #[cfg(not(unix))]
                ChildHandle::Pipe(child) => match child.try_wait() {
                    Ok(Some(_)) => return true,
                    Ok(None) => return false,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => return false,
                },
                ChildHandle::Pty(child) => match child.try_wait() {
                    Ok(Some(_)) => return true,
                    Ok(None) => return false,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        #[cfg(unix)]
                        {
                            return error.raw_os_error() == Some(libc::ECHILD);
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = error;
                            return false;
                        }
                    }
                },
            }
        }
    }

    /// GNU's `kill (- p->pid, SIGKILL)` (src/callproc.c:206), with this port's
    /// existing fallback to the handle's own kill when the group does not take
    /// it.
    fn kill_group_then_handle(&mut self, signal_num: i32) {
        if super::sys::send_signal_to_group(self.pid as i64, signal_num) == 0 {
            return;
        }
        self.kill_handle();
    }

    fn kill_handle(&mut self) {
        match &mut self.handle {
            ChildHandle::Pipe(child) => {
                let _ = child.kill();
            }
            ChildHandle::Pty(child) => {
                let _ = child.kill();
            }
        }
    }
}

impl std::fmt::Debug for ChildOwnership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAChild => f.write_str("NotAChild"),
            Self::Unreaped(child) => write!(f, "Unreaped({})", child.pid),
            Self::Reaped => f.write_str("Reaped"),
            Self::Deferred => f.write_str("Deferred"),
        }
    }
}

#[cfg(test)]
#[path = "tests/deferred_reap.rs"]
mod deferred_reap_tests;
