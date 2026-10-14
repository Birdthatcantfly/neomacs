//! GNU's thread primitives in a Green World once its runtime started
//! (ported from F4's `facade_thread_operation`, with GNU's rules from
//! `src/thread.c`). Each runs on the thread holding the Baton, whose
//! `Context::threads` is the World's `ThreadManager`.

use super::{ThreadKey, Wait};
use crate::emacs_core::error::{EvalResult, expect_args, signal};
use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;

impl Context {
    /// Whether thread primitives take the Green path: this owner runs in a
    /// started Green runtime.
    pub(crate) fn green_threads_running(&mut self) -> bool {
        self.green_seat().is_some()
    }

    /// The running thread's id in the World's `ThreadManager`.
    fn green_current_id(&mut self) -> u64 {
        self.green_key().map_or(0, ThreadKey::id)
    }

    /// `(thread-yield)`: let every other ready thread run first.
    pub(crate) fn green_thread_yield(&mut self, args: &[Value]) -> EvalResult {
        expect_args("thread-yield", args, 0)?;
        self.green_wait(Wait::Runnable)?;
        Ok(Value::NIL)
    }

    /// `(thread-join THREAD)` (GNU `Fthread_join`, thread.c:1118-1144):
    /// wait for THREAD to end and return its result. An error another
    /// thread injected into THREAD with `thread-signal` is re-raised here,
    /// as snapshotted before the wait; an error THREAD signalled itself is
    /// only `thread-last-error`.
    pub(crate) fn green_thread_join(&mut self, args: &[Value]) -> EvalResult {
        expect_args("thread-join", args, 1)?;
        let id = crate::emacs_core::threads::expect_thread_id(&self.threads, &args[0])?;
        if id == self.green_current_id() {
            return Err(signal(
                "error",
                vec![Value::string("Cannot join current thread")],
            ));
        }
        let pending = self.threads.pending_thread_signal(id);
        let key = ThreadKey::from_id(id);
        let control = self.with_green_runtime(|runtime| std::sync::Arc::clone(&runtime.control));
        while !control.exited(key) {
            self.green_wait(Wait::Join(key))?;
        }
        self.threads.join_thread(id);
        if let Some(flow) = pending {
            return Err(flow);
        }
        Ok(self.threads.thread_result(id))
    }
}

/// `make-thread` in a Green World.
pub(crate) fn green_make_thread(ctx: &mut Context, args: &[Value]) -> EvalResult {
    ctx.green_spawn(args)
}

/// Whether `make-thread` takes the Green path: the World is Green.
pub(crate) fn green_world(ctx: &Context) -> bool {
    ctx.threading_mode() == super::super::ThreadingMode::Green
}
