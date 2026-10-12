//! GEN1 card table (card-mark Stage A, tmp/v10x/reports/cardmark-stageA-2026-10-12.md).
//!
//! One masked byte per 512 bytes of owner address space. A store that may
//! create an old-to-young edge marks the card of its OWNER's start address;
//! the minor collection consults dirty cards. The index wraps at the mask, so
//! two owners 4 GiB apart share a byte: aliasing can only make a card look
//! dirty, never hide an edge. Clean is zero so the table starts as untouched,
//! lazily zero-filled pages.
//!
//! Threading: mutators only ever store [`CARD_DIRTY`] into a byte (idempotent,
//! Relaxed, no read-modify-write); the collector reads and clears bytes only
//! with every mutator stopped.

use std::alloc::{self, Layout};
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, Ordering};

/// log2 of the card size: 512 bytes, 32 cons cells.
pub(crate) const CARD_SHIFT: u32 = 9;
pub(crate) const CARD_BYTES: usize = 1 << CARD_SHIFT;
/// Index bits: 2^23 one-byte cards, an aliasing period of 4 GiB.
pub(crate) const CARD_INDEX_BITS: u32 = 23;
pub(crate) const CARD_INDEX_MASK: usize = (1 << CARD_INDEX_BITS) - 1;
pub(crate) const CARD_TABLE_BYTES: usize = 1 << CARD_INDEX_BITS;
/// Cons blocks and arena pages are 64 KiB-aligned chunks of 128 cards.
pub(crate) const CHUNK_BYTES: usize = 64 * 1024;
pub(crate) const CHUNK_CARDS: usize = CHUNK_BYTES / CARD_BYTES;
pub(crate) const CARD_CLEAN: u8 = 0;
pub(crate) const CARD_DIRTY: u8 = 1;

/// The card of an owner address. Total: any address names exactly one card.
#[inline(always)]
pub(crate) const fn card_index(owner_addr: usize) -> usize {
    (owner_addr >> CARD_SHIFT) & CARD_INDEX_MASK
}

/// Heap-owned zeroed byte table; its address is fixed for the heap's life.
pub(crate) struct CardTable {
    bytes: NonNull<AtomicU8>,
}

// SAFETY: the table is a fixed allocation of atomic bytes; every access goes
// through `AtomicU8`, and the allocation lives until the owning heap drops it.
unsafe impl Send for CardTable {}
unsafe impl Sync for CardTable {}

impl CardTable {
    fn layout() -> Layout {
        Layout::from_size_align(CARD_TABLE_BYTES, 64).expect("card table layout")
    }

    pub(crate) fn new() -> Self {
        let layout = Self::layout();
        // SAFETY: non-zero size; zero is CARD_CLEAN for every byte.
        let raw = unsafe { alloc::alloc_zeroed(layout) };
        let bytes = NonNull::new(raw.cast::<AtomicU8>())
            .unwrap_or_else(|| alloc::handle_alloc_error(layout));
        Self { bytes }
    }

    #[inline(always)]
    fn byte(&self, index: usize) -> &AtomicU8 {
        debug_assert!(index < CARD_TABLE_BYTES);
        // SAFETY: `index` is masked into the allocation, which outlives `self`.
        unsafe { &*self.bytes.as_ptr().add(index) }
    }

    /// Mark the card of OWNER_ADDR. Mutator side: one Relaxed byte store.
    #[inline(always)]
    pub(crate) fn mark(&self, owner_addr: usize) {
        self.byte(card_index(owner_addr))
            .store(CARD_DIRTY, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn is_dirty(&self, owner_addr: usize) -> bool {
        self.byte(card_index(owner_addr)).load(Ordering::Relaxed) != CARD_CLEAN
    }

    /// Whether any card of the 64 KiB chunk at CHUNK_BASE is dirty: its 128
    /// card bytes are contiguous in the table, read as sixteen words.
    /// Collector side, with every mutator stopped.
    #[inline]
    pub(crate) fn chunk_has_dirty(&self, chunk_base: usize) -> bool {
        const WORDS: usize = CHUNK_CARDS / size_of::<u64>();
        debug_assert_eq!(chunk_base % CHUNK_BYTES, 0);
        let first = card_index(chunk_base);
        // SAFETY: a chunk's cards are CHUNK_CARDS consecutive in-table bytes
        // starting at a CHUNK_CARDS-aligned index, so the words are aligned
        // and in bounds; every mutator is stopped, so no store races.
        let words = unsafe { self.bytes.as_ptr().add(first).cast::<u64>() };
        (0..WORDS).any(|word| unsafe { words.add(word).read() } != 0)
    }

    /// Collector side, with every mutator stopped.
    #[inline]
    pub(crate) fn clear(&self, owner_addr: usize) {
        self.byte(card_index(owner_addr))
            .store(CARD_CLEAN, Ordering::Relaxed);
    }
}

impl Drop for CardTable {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with the same layout.
        unsafe { alloc::dealloc(self.bytes.as_ptr().cast(), Self::layout()) };
    }
}
