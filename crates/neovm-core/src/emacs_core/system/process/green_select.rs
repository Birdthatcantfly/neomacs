//! The Green scheduler's owned OS wait handle. It carries no process map,
//! Lisp value, or World borrow: only the poller and its wakeup metadata.

use super::{Arc, AtomicBool, Instant, Ordering, ProcessManager};
use std::sync::{Mutex, PoisonError};

pub(crate) struct GreenSelectHandle {
    poller: Arc<polling::Poller>,
    notification_pending: Arc<AtomicBool>,
    /// Reused for polling at every safepoint while an owner selects.
    events: Mutex<polling::Events>,
    #[cfg(unix)]
    signal_fd: Option<std::os::fd::RawFd>,
}

static_assertions::assert_impl_all!(GreenSelectHandle: Send, Sync);

impl GreenSelectHandle {
    /// Scheduler events are remembered even before the idle select starts.
    pub(crate) fn notify(&self) {
        self.notification_pending.store(true, Ordering::Release);
        let _ = self.poller.notify();
    }

    /// Wait without touching World storage. True means descriptor readiness
    /// or a notification; false means the absolute deadline has elapsed.
    /// Only the idle scheduler calls this, so the shared poller has one waiter.
    pub(crate) fn wait_until(&self, deadline: Instant) -> bool {
        if crate::emacs_core::os_signal::pending()
            || self.notification_pending.swap(false, Ordering::AcqRel)
        {
            return true;
        }
        #[cfg(unix)]
        if let Some(fd) = self.signal_fd {
            // SAFETY: the OS-signal installation owns this descriptor for
            // the process lifetime, longer than this handle's registration.
            let source = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            if self
                .poller
                .modify(source, polling::Event::readable(usize::MAX - 1))
                .is_err()
            {
                return true;
            }
        }
        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            events.clear();
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.poller.wait(&mut events, Some(remaining)) {
                Ok(_) => {
                    let notification = self.notification_pending.swap(false, Ordering::AcqRel);
                    if notification || !events.is_empty() {
                        return true;
                    }
                    if Instant::now() >= deadline {
                        return false;
                    }
                    // Poller permits eventless, spurious wakes. Recheck the
                    // same absolute deadline rather than shortening the wait.
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                // Reacquire the Baton and let the ordinary wait backend
                // handle its degraded fallback on an OS/backend error.
                Err(_) => return true,
            }
        }
    }
}

impl ProcessManager {
    /// Capture while holding the Baton. The scheduler subsequently observes
    /// only OS readiness; the resumed owner polls live processes under its
    /// own wait policy, so an event consumed here loses no process bytes.
    pub(crate) fn green_select_handle(&self) -> Option<GreenSelectHandle> {
        Some(GreenSelectHandle {
            poller: Arc::clone(self.wait_backend.poller.as_ref()?),
            notification_pending: Arc::clone(&self.wait_backend.notification_pending),
            events: Mutex::new(polling::Events::new()),
            #[cfg(unix)]
            signal_fd: self.wait_backend.signal_fd,
        })
    }
}
