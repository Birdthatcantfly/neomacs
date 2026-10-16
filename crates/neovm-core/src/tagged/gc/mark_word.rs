//! Collector work words: Lisp values as mark queues carry them.
//!
//! A raw `TaggedValue` is confined to its mutator thread. The concurrent
//! marker is not a mutator; it receives gray and SATB work under the
//! collection protocol, where objects stay alive through the cycle's roots,
//! marks and barriers rather than through the queued words themselves. These
//! types carry that work across the thread boundary and turn back into values
//! only inside the collector.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use crate::tagged::value::TaggedValue;

/// One value word queued for the collector.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(super) struct MarkWord(usize);

static_assertions::assert_impl_all!(MarkWord: Send, Sync, Copy, std::fmt::Debug);
static_assertions::assert_eq_size!(MarkWord, TaggedValue);

impl MarkWord {
    #[inline(always)]
    pub(super) fn of(value: TaggedValue) -> Self {
        Self(value.bits())
    }

    /// The value, for collector code on the marker or on a world-stopped
    /// mutator.
    #[inline(always)]
    pub(super) fn value(self) -> TaggedValue {
        TaggedValue::from_bits(self.0)
    }
}

impl std::fmt::Debug for MarkWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MarkWord({:#x})", self.0)
    }
}

/// A queue shared between mutator SATB barriers and the marker thread.
/// Multiple producers serialize through `words`; the single marker checks
/// `pending` before acquiring the mutex, avoiding writes while it is idle.
#[derive(Debug, Default)]
pub(super) struct MarkQueue {
    pending: AtomicUsize,
    words: Mutex<Vec<MarkWord>>,
}

static_assertions::assert_impl_all!(MarkQueue: Send, Sync, std::fmt::Debug);

impl MarkQueue {
    /// Keep existing push/fold callers behind one guard. Publishing the length
    /// in its destructor prevents a caller from forgetting the pending update.
    pub(super) fn lock(&self) -> LockResult<MarkQueueGuard<'_>> {
        match self.words.lock() {
            Ok(words) => Ok(MarkQueueGuard { queue: self, words }),
            Err(poison) => Err(PoisonError::new(MarkQueueGuard {
                queue: self,
                words: poison.into_inner(),
            })),
        }
    }

    pub(super) fn is_poisoned(&self) -> bool {
        self.words.is_poisoned()
    }

    /// An idle drain only reads the pending count. Poison must remain visible
    /// even when an unwinding producer left an empty queue: skipping the mutex
    /// must never conceal failed collector state. The Acquire pairs with every
    /// guard's Release publication; mutex ownership still protects the words.
    pub(super) fn take_all(&self) -> Result<Vec<MarkWord>, PoisonError<MarkQueueGuard<'_>>> {
        if !self.is_poisoned() && self.pending.load(Ordering::Acquire) == 0 {
            return Ok(Vec::new());
        }
        self.lock().map(|mut words| std::mem::take(&mut *words))
    }
}

/// While held, this guard owns all queued words. Its Drop publishes their
/// final count before the underlying mutex guard unlocks, so a following
/// producer cannot have its update overwritten by an earlier owner's Drop.
#[derive(Debug)]
#[must_use = "the queue stays locked until this guard is dropped"]
pub(super) struct MarkQueueGuard<'a> {
    queue: &'a MarkQueue,
    words: MutexGuard<'a, Vec<MarkWord>>,
}

static_assertions::assert_impl_all!(MarkQueueGuard<'static>: Sync, std::fmt::Debug);
static_assertions::assert_not_impl_any!(MarkQueueGuard<'static>: Send);

impl Deref for MarkQueueGuard<'_> {
    type Target = Vec<MarkWord>;

    fn deref(&self) -> &Self::Target {
        &self.words
    }
}

impl DerefMut for MarkQueueGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.words
    }
}

impl Drop for MarkQueueGuard<'_> {
    fn drop(&mut self) {
        // `words` remains locked until field destruction after this method.
        self.queue
            .pending
            .store(self.words.len(), Ordering::Release);
    }
}

pub(super) type SharedMarkQueue = Arc<MarkQueue>;

#[cfg(test)]
#[path = "tests/mark_queue_test.rs"]
mod tests;

/// The marker thread's own gray stack.
#[derive(Default)]
pub(super) struct MarkStack(Vec<MarkWord>);

static_assertions::assert_impl_all!(MarkStack: Send, std::fmt::Debug);

impl MarkStack {
    /// Take over a world-stopped gray queue at the start handshake.
    pub(super) fn from_values(values: Vec<TaggedValue>) -> Self {
        Self(values.into_iter().map(MarkWord::of).collect())
    }

    #[inline(always)]
    pub(super) fn push(&mut self, value: TaggedValue) {
        self.0.push(MarkWord::of(value));
    }

    #[inline(always)]
    pub(super) fn pop(&mut self) -> Option<TaggedValue> {
        self.0.pop().map(MarkWord::value)
    }

    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Append a batch drained from a shared queue.
    pub(super) fn extend_words(&mut self, words: Vec<MarkWord>) {
        self.0.extend(words);
    }

    /// Hand every remaining word to a shared queue.
    pub(super) fn drain_words(&mut self) -> std::vec::Drain<'_, MarkWord> {
        self.0.drain(..)
    }

    /// The remaining work as values, for collector tests.
    #[cfg(test)]
    pub(super) fn into_values(self) -> Vec<TaggedValue> {
        self.0.into_iter().map(MarkWord::value).collect()
    }
}

impl std::fmt::Debug for MarkStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarkStack")
            .field("len", &self.0.len())
            .finish()
    }
}
