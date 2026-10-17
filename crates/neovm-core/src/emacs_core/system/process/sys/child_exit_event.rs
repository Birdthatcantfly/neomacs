//! Native exit notifications for deleted children, independent of any evaluator.
//!
//! Registration occurs while the caller still owns an unreaped child, before
//! publishing it to the deferred registry. The native source therefore tracks
//! that child even if another mutator reaps it before the watcher starts. A
//! watcher owns only this source; the registry retains sole waitpid authority.

use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug)]
pub enum ChildExitEvent {
    #[cfg(target_os = "linux")]
    Linux(OwnedFd),
    #[cfg(target_os = "macos")]
    Darwin(OwnedFd),
}

impl ChildExitEvent {
    #[cfg(target_os = "linux")]
    pub fn open(pid: u32) -> io::Result<Self> {
        // SAFETY: pid is the caller's exclusively owned, unreaped child.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pidfd_open returned a fresh descriptor, with CLOEXEC set.
        Ok(Self::Linux(unsafe {
            OwnedFd::from_raw_fd(fd as libc::c_int)
        }))
    }

    #[cfg(target_os = "macos")]
    pub fn open(pid: u32) -> io::Result<Self> {
        // SAFETY: kqueue takes no pointers and returns a fresh descriptor.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: kqueue returned a fresh descriptor owned by this value.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: fd is live; F_SETFD takes an integer flag argument.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // XNU emits NOTE_EXIT before the child becomes waitable, then sends
        // SIGCHLD after marking SZOMB (kern_exit.c:2353-2354, 2421-2426).
        // Register the signal edge first to cover that boundary and a child
        // whose NOTE_EXIT already occurred. EVFILT_SIGNAL leaves the process's
        // signal handler/disposition unchanged.
        let signal = libc::kevent {
            ident: libc::SIGCHLD as libc::uintptr_t,
            filter: libc::EVFILT_SIGNAL,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        loop {
            // SAFETY: one initialized change; no output events requested.
            let result = unsafe {
                libc::kevent(
                    fd.as_raw_fd(),
                    &signal,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if result >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        let change = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        loop {
            // SAFETY: change is initialized and readable for one event;
            // null output/timeout pointers are valid with zero output events.
            let result = unsafe {
                libc::kevent(
                    fd.as_raw_fd(),
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if result >= 0 {
                return Ok(Self::Darwin(fd));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // The process may have left the event population. Its wait status
            // is collected before sleeping, or the registered SIGCHLD wakes us
            // once the pending exit becomes waitable.
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(Self::Darwin(fd));
            }
            return Err(error);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn open(_pid: u32) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native deleted-child exit source unavailable",
        ))
    }

    /// Wait only on a native notification source, never on waitpid. EINTR
    /// resumes the same wait; no timeout, sleep, or process-status polling runs.
    pub fn wait(&self) -> io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(fd) => {
                let mut event = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                loop {
                    // SAFETY: event is one writable pollfd; fd remains owned
                    // throughout this wait, and -1 requests an event-only wait.
                    let result = unsafe { libc::poll(&mut event, 1, -1) };
                    if result > 0 {
                        return if event.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                            Ok(())
                        } else {
                            Err(io::Error::other(
                                "deleted-child pidfd reported an invalid event",
                            ))
                        };
                    }
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
            #[cfg(target_os = "macos")]
            Self::Darwin(fd) => loop {
                let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
                // SAFETY: event has writable storage for one kevent; the fd
                // remains owned, and null timeout requests an event-only wait.
                let result = unsafe {
                    libc::kevent(
                        fd.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        event.as_mut_ptr(),
                        1,
                        std::ptr::null(),
                    )
                };
                if result > 0 {
                    // SAFETY: a positive result initialized our single event.
                    let event = unsafe { event.assume_init() };
                    if event.flags & libc::EV_ERROR != 0 {
                        return Err(io::Error::from_raw_os_error(event.data as libc::c_int));
                    }
                    return Ok(());
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            },
        }
    }
}
