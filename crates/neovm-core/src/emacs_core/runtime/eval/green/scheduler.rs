//! Who holds the Baton next (ported from F4's `Control`, research branch
//! `facade_threads.rs`).
//!
//! Each Lisp thread has a key and a channel; a parked thread blocks in its
//! channel's receive until the Baton arrives. The holder hands the Baton on
//! only at a switch point: it records what it waits for, picks the first
//! ready thread (FIFO, as GNU's runnable queue) and sends it the Baton.
//! A thread waiting for a mutex, a condition, another thread's end or a
//! deadline is not ready until another thread makes it so.

use super::super::world::Baton;
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU64;
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// A Lisp thread's identity in the scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ThreadKey {
    /// The thread the World started on (GNU's `main-thread`).
    Main,
    Worker(NonZeroU64),
}

impl ThreadKey {
    /// The thread's id in the World's `ThreadManager` (0 for main).
    pub(crate) fn id(self) -> u64 {
        match self {
            Self::Main => 0,
            Self::Worker(id) => id.get(),
        }
    }

    /// The key of the thread with ID in the World's `ThreadManager`.
    pub(crate) fn from_id(id: u64) -> Self {
        NonZeroU64::new(id).map_or(Self::Main, Self::Worker)
    }
}

/// What a thread that gave the Baton up waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// Nothing: it runs again in its turn.
    Runnable,
    /// Another thread's end (`thread-join`).
    Join(ThreadKey),
    /// A mutex another thread holds (`mutex-lock`).
    Mutex(u64),
    /// A condition variable's notification (`condition-wait`).
    Condition(u64),
    /// A `thread_select` wait that gave the Baton up for a slice: runnable
    /// again at `slice_end`, to watch its own input; its wait itself ends
    /// at `wait_end`, which is all a waiting peer blocks for.
    Select {
        slice_end: Instant,
        wait_end: Instant,
    },
    /// It ended.
    Exited,
}

/// What the other threads are doing, seen by a thread about to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Peers {
    /// One can run now.
    Ready,
    /// None can run before this deadline, the earliest one sleeps to.
    Sleeping(Instant),
    /// None can run until a running thread wakes it.
    Blocked,
}

/// Whether the runtime still runs threads, or is shutting down because its
/// main thread ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Running,
    Closing,
}

/// Why a hand-off did not happen.
#[derive(Debug)]
pub(crate) enum Handoff {
    /// No other thread can run and the caller waits for something only a
    /// running thread could give it: GNU would hang (neomacs signals).
    NoRunnable(Baton),
}

#[derive(Default)]
struct ControlState {
    senders: BTreeMap<ThreadKey, mpsc::Sender<Baton>>,
    waits: BTreeMap<ThreadKey, Wait>,
    ready: VecDeque<ThreadKey>,
    joins: BTreeMap<ThreadKey, std::thread::JoinHandle<()>>,
    closing: bool,
}

/// The scheduler, shared by every thread's seat.
#[derive(Default)]
pub(crate) struct Control {
    state: Mutex<ControlState>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control").finish_non_exhaustive()
    }
}

impl Control {
    fn state(&self) -> MutexGuard<'_, ControlState> {
        // Every update is a few map/queue operations that leave the state
        // consistent; a panic while holding the lock cannot tear it.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register the main thread, which holds the Baton: its channel.
    pub(crate) fn register_main(&self) -> mpsc::Receiver<Baton> {
        let (sender, receiver) = mpsc::channel();
        let mut state = self.state();
        state.senders.insert(ThreadKey::Main, sender);
        state.waits.insert(ThreadKey::Main, Wait::Runnable);
        receiver
    }

    /// Register the new thread KEY, ready to run in its turn: its channel.
    pub(crate) fn register_worker(&self, key: ThreadKey) -> mpsc::Receiver<Baton> {
        let (sender, receiver) = mpsc::channel();
        let mut state = self.state();
        state.senders.insert(key, sender);
        state.waits.insert(key, Wait::Runnable);
        state.ready.push_back(key);
        receiver
    }

    /// Keep the OS thread KEY runs on, to join at shutdown.
    pub(crate) fn note_join_handle(&self, key: ThreadKey, handle: std::thread::JoinHandle<()>) {
        self.state().joins.insert(key, handle);
    }

    /// Whether KEY has ended.
    pub(crate) fn exited(&self, key: ThreadKey) -> bool {
        matches!(self.state().waits.get(&key), Some(Wait::Exited) | None)
    }

    /// Whether a thread other than ME is ready to run, or will be at a
    /// deadline.
    pub(crate) fn others_could_run(&self, me: ThreadKey) -> bool {
        self.state()
            .waits
            .iter()
            .any(|(key, wait)| *key != me && matches!(wait, Wait::Runnable | Wait::Select { .. }))
    }

    /// What the threads other than ME are doing at NOW.
    pub(crate) fn peers(&self, me: ThreadKey, now: Instant) -> Peers {
        let state = self.state();
        let mut earliest: Option<Instant> = None;
        for (key, wait) in &state.waits {
            if *key == me {
                continue;
            }
            match *wait {
                Wait::Runnable => return Peers::Ready,
                Wait::Select { wait_end, .. } if wait_end <= now => return Peers::Ready,
                Wait::Select {
                    wait_end: deadline, ..
                } => {
                    earliest = Some(earliest.map_or(deadline, |e| e.min(deadline)));
                }
                _ => {}
            }
        }
        earliest.map_or(Peers::Blocked, Peers::Sleeping)
    }

    /// Make KEY the next thread to run, whatever it waits for.
    pub(crate) fn prioritize(&self, key: ThreadKey) {
        let mut state = self.state();
        if matches!(state.waits.get(&key), Some(Wait::Exited) | None) {
            return;
        }
        state.ready.retain(|ready| *ready != key);
        state.waits.insert(key, Wait::Runnable);
        state.ready.push_front(key);
    }

    /// What KEY waits for.
    pub(crate) fn wait_of(&self, key: ThreadKey) -> Option<Wait> {
        self.state().waits.get(&key).copied()
    }

    /// Whether the runtime is shutting down.
    pub(crate) fn lifecycle(&self) -> Lifecycle {
        if self.state().closing {
            Lifecycle::Closing
        } else {
            Lifecycle::Running
        }
    }

    /// Make KEY runnable again if it waits for something.
    pub(crate) fn wake(&self, key: ThreadKey) {
        let mut state = self.state();
        Self::wake_locked(&mut state, key);
    }

    /// Make every thread waiting for WAIT runnable (a mutex release, a
    /// notification to all, a thread's end).
    pub(crate) fn wake_waiting_for(&self, wait: Wait) -> usize {
        let mut state = self.state();
        let keys: Vec<ThreadKey> = state
            .waits
            .iter()
            .filter(|(_, w)| **w == wait)
            .map(|(key, _)| *key)
            .collect();
        for key in &keys {
            Self::wake_locked(&mut state, *key);
        }
        keys.len()
    }

    /// Make the first thread (in key order) waiting for WAIT runnable.
    pub(crate) fn wake_one_waiting_for(&self, wait: Wait) -> bool {
        let mut state = self.state();
        let key = state
            .waits
            .iter()
            .find(|(_, w)| **w == wait)
            .map(|(key, _)| *key);
        match key {
            Some(key) => {
                Self::wake_locked(&mut state, key);
                true
            }
            None => false,
        }
    }

    fn wake_locked(state: &mut ControlState, key: ThreadKey) {
        match state.waits.get(&key) {
            Some(Wait::Runnable | Wait::Exited) | None => {}
            Some(_) => {
                state.waits.insert(key, Wait::Runnable);
                state.ready.push_back(key);
            }
        }
    }

    /// Move threads whose deadline passed into the ready queue.
    fn expire_deadlines(state: &mut ControlState, now: Instant) {
        let expired: Vec<ThreadKey> = state
            .waits
            .iter()
            .filter(|(_, wait)| matches!(wait, Wait::Select { slice_end, .. } if *slice_end <= now))
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            Self::wake_locked(state, key);
        }
    }

    /// The next thread to hold the Baton, if any can run; with no ready
    /// thread but one waiting for a deadline, the earliest deadline.
    fn pick(state: &mut ControlState) -> Option<(ThreadKey, Option<Instant>)> {
        Self::expire_deadlines(state, Instant::now());
        while let Some(key) = state.ready.pop_front() {
            if state.waits.get(&key) == Some(&Wait::Runnable) {
                return Some((key, None));
            }
        }
        state
            .waits
            .iter()
            .filter_map(|(key, wait)| match wait {
                Wait::Select { slice_end, .. } => Some((*key, *slice_end)),
                _ => None,
            })
            .min_by_key(|(_, deadline)| *deadline)
            .map(|(key, deadline)| (key, Some(deadline)))
    }

    /// Hand the Baton on from ME, which waits for WAIT, and block until it
    /// comes back. Returns it at once when ME is the next to run.
    pub(crate) fn transfer(
        &self,
        me: ThreadKey,
        wait: Wait,
        baton: Baton,
        receiver: &mpsc::Receiver<Baton>,
    ) -> Result<Baton, Handoff> {
        let (next, deadline, sender) = {
            let mut state = self.state();
            state.waits.insert(me, wait);
            if wait == Wait::Runnable {
                state.ready.push_back(me);
            }
            let Some((next, deadline)) = Self::pick(&mut state) else {
                state.waits.insert(me, Wait::Runnable);
                return Err(Handoff::NoRunnable(baton));
            };
            if next == me {
                state.waits.insert(me, Wait::Runnable);
                drop(state);
                if let Some(deadline) = deadline {
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                }
                return Ok(baton);
            }
            let sender = state
                .senders
                .get(&next)
                .cloned()
                .expect("every registered thread has a channel");
            (next, deadline, sender)
        };
        if let Some(deadline) = deadline {
            // Nothing can run before NEXT's deadline: wait it out here,
            // holding the Baton, then let NEXT run.
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            self.state().waits.insert(next, Wait::Runnable);
        }
        if sender.send(baton).is_err() {
            // NEXT's OS thread is gone with its receiver: nothing can hand
            // the Baton back, and the World's Lisp would stop.
            std::process::abort();
        }
        match receiver.recv() {
            Ok(baton) => {
                baton.enter();
                Ok(baton)
            }
            Err(_) => std::process::abort(),
        }
    }

    /// ME ended: hand the Baton to the next thread, wake its joiners, and
    /// return without waiting. With no thread able to run, the runtime is
    /// closing: every waiting thread is woken (it signals) and gets the
    /// Baton in turn.
    pub(crate) fn finish(&self, me: ThreadKey, baton: Baton) {
        let sender = {
            let mut state = self.state();
            state.waits.insert(me, Wait::Exited);
            let joiners: Vec<ThreadKey> = state
                .waits
                .iter()
                .filter(|(_, wait)| **wait == Wait::Join(me))
                .map(|(key, _)| *key)
                .collect();
            for key in joiners {
                Self::wake_locked(&mut state, key);
            }
            let next = match Self::pick(&mut state) {
                Some((next, _)) => Some(next),
                None => {
                    state.closing = true;
                    let blocked: Vec<ThreadKey> = state
                        .waits
                        .iter()
                        .filter(|(_, wait)| !matches!(wait, Wait::Exited))
                        .map(|(key, _)| *key)
                        .collect();
                    for key in blocked {
                        Self::wake_locked(&mut state, key);
                    }
                    Self::pick(&mut state).map(|(next, _)| next)
                }
            };
            next.and_then(|next| state.senders.get(&next).cloned())
        };
        match sender {
            Some(sender) => {
                if sender.send(baton).is_err() {
                    std::process::abort();
                }
            }
            // Nothing else lives: the Baton (and the World's facade epoch)
            // goes with the last thread.
            None => drop(baton),
        }
    }

    /// Start closing: every waiting thread is woken, to signal and end.
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.closing = true;
        let blocked: Vec<ThreadKey> = state
            .waits
            .iter()
            .filter(|(_, wait)| !matches!(wait, Wait::Exited | Wait::Runnable))
            .map(|(key, _)| *key)
            .collect();
        for key in blocked {
            Self::wake_locked(&mut state, key);
        }
    }

    /// Take KEY's join handle, once KEY has ended.
    pub(crate) fn take_join_handle(&self, key: ThreadKey) -> Option<std::thread::JoinHandle<()>> {
        let mut state = self.state();
        if !matches!(state.waits.get(&key), Some(Wait::Exited)) {
            return None;
        }
        state.joins.remove(&key)
    }

    /// Take every worker's join handle (shutdown).
    pub(crate) fn take_join_handles(&self) -> Vec<(ThreadKey, std::thread::JoinHandle<()>)> {
        std::mem::take(&mut self.state().joins)
            .into_iter()
            .collect()
    }
}
