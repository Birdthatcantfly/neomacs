//! Model-only stand-ins. Never imported by neovm-core or its workspace.
#![allow(dead_code)]

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Condvar, Mutex};
use std::marker::PhantomData;
use std::rc::Rc;

pub const WRITER: u64 = 1 << 63;
pub const COUNT_MASK: u64 = (1 << 47) - 1;

/// Mirrors proof 2's reviewed counted-reader/CAS-drain protocol. This is a
/// standalone model, not a new engine helper. C2 must reuse proof 2 once it
/// lands (or temporarily copy behind a deduplication TODO per peer review).
pub struct Authority {
    pub word: AtomicU64,
}

impl Authority {
    pub fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
        }
    }

    /// One attempt, including CAS failure. Caller retries outside all loans,
    /// where a real wrapper can service stop. No spin-cap/fairness assumption.
    pub fn read(&self) -> Option<Reader<'_>> {
        let word = self.word.load(Ordering::Acquire);
        if word & WRITER != 0 || word & COUNT_MASK == COUNT_MASK {
            return None;
        }
        self.word
            .compare_exchange(word, word + 1, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Reader {
            authority: self,
            _confined: PhantomData,
        })
    }

    pub fn write(&self) -> Option<Writer<'_>> {
        self.word
            .compare_exchange(0, WRITER, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Writer {
            authority: self,
            _confined: PhantomData,
        })
    }

    pub fn assert_quiescent(&self) {
        assert_eq!(
            self.word.load(Ordering::Acquire),
            0,
            "authority not quiescent"
        );
    }
}

pub struct Reader<'a> {
    authority: &'a Authority,
    _confined: PhantomData<Rc<()>>,
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        let previous = self.authority.word.fetch_sub(1, Ordering::Release);
        assert_eq!(previous & WRITER, 0, "reader overlaps writer");
        assert!(previous & COUNT_MASK > 0, "reader count underflow");
    }
}

pub struct Writer<'a> {
    authority: &'a Authority,
    _confined: PhantomData<Rc<()>>,
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        assert_eq!(self.authority.word.load(Ordering::Relaxed), WRITER);
        self.authority.word.store(0, Ordering::Release);
    }
}

static_assertions::assert_not_impl_any!(Reader<'static>: Send, Sync, Clone, Copy);
static_assertions::assert_not_impl_any!(Writer<'static>: Send, Sync, Clone, Copy);

/// Checked ordinary storage: synchronization must come from authority, not
/// from atomifying the payload or putting a second mutex around it. Every raw
/// access stays within Loom's tracked with/with_mut lifetime. Broken tests
/// trip Loom's borrow/causality checks before dereferencing conflicting data.
pub struct Checked<T>(UnsafeCell<T>);

// SAFETY: model-only wrapper. All correct callers hold the modeled read/write
// lease, or a matching stopped/joined state. Negative callers deliberately
// violate this; Loom rejects them before the conflicting pointer operation.
unsafe impl<T: Send> Sync for Checked<T> {}

impl<T: Copy> Checked<T> {
    pub fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    pub fn get(&self) -> T {
        self.0.with(|p| {
            // SAFETY: tracked shared access, scoped exactly to this closure.
            unsafe { *p }
        })
    }

    pub fn set(&self, value: T) {
        self.0.with_mut(|p| {
            // SAFETY: tracked exclusive access, scoped exactly to this closure.
            unsafe { *p = value }
        });
    }
}

/// Scripted ordering only. These edges must not be used as evidence for the
/// unscripted cell-publication cases; those rely on the authority word alone.
#[derive(Default)]
pub struct Latch {
    value: Mutex<bool>,
    ready: Condvar,
}

impl Latch {
    pub fn set(&self) {
        *self.value.lock().unwrap() = true;
        self.ready.notify_all();
    }

    pub fn wait(&self) {
        let mut guard = self.value.lock().unwrap();
        while !*guard {
            guard = self.ready.wait(guard).unwrap();
        }
    }
}

/// Two independent plain fields stand for one logical symbol cell. A copy
/// can only be one complete version. The pair is not a seqlock/atomic payload.
pub struct Cell {
    pub value: Checked<usize>,
    pub version: Checked<usize>,
}

impl Cell {
    pub fn new() -> Self {
        Self {
            value: Checked::new(10),
            version: Checked::new(1),
        }
    }

    pub fn copy(&self) -> (usize, usize) {
        (self.value.get(), self.version.get())
    }

    pub fn set(&self, version: usize) {
        self.value.set(version * 10);
        self.version.set(version);
    }
}
