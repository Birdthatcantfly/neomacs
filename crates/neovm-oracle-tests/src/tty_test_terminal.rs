//! A test-owned pseudo terminal that either editor can open by name.
//!
//! `--batch` oracle forms create a real terminal frame on it with
//! `make-terminal-frame`, which is how a redisplay-level observation (window
//! start, hscroll, cursor row, callback order) is compared with GNU without
//! driving an interactive session.  This is test-local OS state and carries no
//! Lisp values or shared mutator state.  A reader thread drains output so full
//! redisplays cannot fill the PTY buffer, and keeps it for the forms whose
//! observation is the byte stream itself (what term.c writes, or does not
//! write, to the terminal); release/acquire ordering publishes its stop
//! request independently of Lisp.

#![allow(dead_code)]

use std::ffi::CStr;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Owns both ends while either editor opens this terminal by name.
#[must_use]
#[derive(Debug)]
pub(crate) struct TestTerminal {
    _slave: OwnedFd,
    pub(crate) name: String,
    stop_reader: Arc<AtomicBool>,
    /// Everything the editor wrote to the terminal so far.
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl TestTerminal {
    /// The 80x25 terminal most oracle forms are content with.
    pub(crate) fn new() -> Self {
        Self::with_size(25, 80)
    }

    /// A terminal of a chosen size, for forms whose observation depends on
    /// the window geometry (a viewport decision, an auto-hscroll).
    pub(crate) fn with_size(rows: u16, cols: u16) -> Self {
        let mut master = -1;
        let mut slave = -1;
        let mut name = [0 as libc::c_char; 1024];
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: all output pointers refer to writable initialized storage;
        // the name buffer exceeds the platform's terminal-name limit, and the
        // optional termios pointer is null as permitted by openpty. Darwin
        // declares the termios and winsize parameters as `*mut` while Linux
        // declares them `*const`; mutable pointers coerce to both.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(
            result,
            0,
            "open test TTY: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful openpty returned two distinct owned descriptors.
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        // SAFETY: the slave descriptor is independently owned after openpty.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        // SAFETY: successful openpty wrote a null-terminated terminal name.
        let name = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_str()
            .expect("UTF-8 test TTY name")
            .to_owned();
        // SAFETY: master is a live owned descriptor. F_GETFL has no pointer
        // arguments, and F_SETFL accepts the flags returned by F_GETFL.
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "read PTY flags");
        // SAFETY: master remains live and flags came from this descriptor.
        let result =
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(result, 0, "make test PTY reader nonblocking");
        let stop_reader = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stop_reader);
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let reader = std::thread::spawn(move || {
            let mut master = std::fs::File::from(master);
            let mut bytes = [0u8; 8192];
            while !stop.load(Ordering::Acquire) {
                let mut poll = libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: poll points to one initialized pollfd entry whose
                // owned descriptor remains live throughout the call.
                if unsafe { libc::poll(&mut poll, 1, 50) } <= 0 {
                    continue;
                }
                match master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => sink
                        .lock()
                        .expect("test PTY output lock")
                        .extend_from_slice(&bytes[..n]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            _slave: slave,
            name,
            stop_reader,
            output,
            reader: Some(reader),
        }
    }

    pub(crate) fn finish(mut self) {
        self.stop_reader.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            reader.join().expect("join test PTY reader");
        }
    }

    /// Stop reading and return everything the editor wrote.  The editor has
    /// exited by the time a form's result is in hand, so the stream is
    /// complete once the reader thread has drained the PTY.
    pub(crate) fn finish_with_output(mut self) -> Vec<u8> {
        self.stop_reader.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            reader.join().expect("join test PTY reader");
        }
        std::mem::take(&mut *self.output.lock().expect("test PTY output lock"))
    }

    /// Which of `tokens` the editor drew, in order of first appearance: the
    /// text of `output` with its control sequences removed (CSI, OSC, other
    /// ESC sequences, C0 controls).  A token drawn as one run of glyphs is
    /// found whatever cursor motions surround it; one the encoder split
    /// inside is not, so tokens must be words the editor writes whole.
    pub(crate) fn drawn_tokens<'a>(output: &[u8], tokens: &[&'a str]) -> Vec<&'a str> {
        let mut text = Vec::with_capacity(output.len());
        let mut rest = output;
        while let Some((&byte, after)) = rest.split_first() {
            rest = after;
            if byte != 0x1b {
                if byte >= 0x20 && byte != 0x7f {
                    text.push(byte);
                }
                continue;
            }
            match rest.first() {
                Some(b'[') => {
                    // CSI: parameters and intermediates, then one final byte.
                    let end = rest[1..]
                        .iter()
                        .position(|b| (0x40..=0x7e).contains(b))
                        .map_or(rest.len(), |at| at + 2);
                    rest = &rest[end.min(rest.len())..];
                }
                Some(b']') => {
                    // OSC: up to BEL or ESC backslash.
                    let mut at = 1;
                    while at < rest.len() {
                        if rest[at] == 0x07 {
                            at += 1;
                            break;
                        }
                        if rest[at] == 0x1b && rest.get(at + 1) == Some(&b'\\') {
                            at += 2;
                            break;
                        }
                        at += 1;
                    }
                    rest = &rest[at.min(rest.len())..];
                }
                Some(_) => rest = &rest[1..],
                None => {}
            }
        }
        let text = String::from_utf8_lossy(&text);
        let mut found: Vec<(usize, &'a str)> = tokens
            .iter()
            .filter_map(|&token| text.find(token).map(|at| (at, token)))
            .collect();
        found.sort_unstable();
        found.into_iter().map(|(_, token)| token).collect()
    }

    /// [`Self::drawn_tokens`] for what the editor drew after the last
    /// occurrence of `marker` (a string the form writes to the terminal with
    /// `send-string-to-terminal` between two redisplays); everything before
    /// it when the marker never appears.
    pub(crate) fn drawn_tokens_after<'a>(
        output: &[u8],
        marker: &str,
        tokens: &[&'a str],
    ) -> Vec<&'a str> {
        let marker = marker.as_bytes();
        let after = output
            .windows(marker.len())
            .rposition(|window| window == marker)
            .map_or(output, |at| &output[at + marker.len()..]);
        Self::drawn_tokens(after, tokens)
    }

    /// The DECSCUSR (`CSI Ps SP q`, cursor style) parameters in `output`, in
    /// order.  GNU's term.c never writes one: on a text terminal the cursor
    /// is whatever the terminal is configured to show.
    pub(crate) fn cursor_style_controls(output: &[u8]) -> Vec<u32> {
        let mut controls = Vec::new();
        let mut rest = output;
        while let Some(at) = rest.windows(2).position(|w| w == b"\x1b[") {
            let after = &rest[at + 2..];
            let digits = after.iter().take_while(|b| b.is_ascii_digit()).count();
            if after.get(digits..digits + 2) == Some(b" q") {
                controls.push(
                    std::str::from_utf8(&after[..digits])
                        .expect("ASCII digits")
                        .parse()
                        .unwrap_or(0),
                );
            }
            rest = after;
        }
        controls
    }
}

impl Drop for TestTerminal {
    fn drop(&mut self) {
        // Unwinding only requests shutdown: Drop never blocks waiting for I/O.
        // Successful tests join explicitly through finish.
        self.stop_reader.store(true, Ordering::Release);
    }
}
