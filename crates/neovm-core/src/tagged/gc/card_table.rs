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
const CHUNK_CARD_WORDS: usize = CHUNK_CARDS / size_of::<u64>();
pub(crate) const CARD_CLEAN: u8 = 0;
pub(crate) const CARD_DIRTY: u8 = 1;

/// Byte offset, from a `*const TaggedHeap`, of the published card-table base
/// (one unaligned native-endian word in byte Cells after the exclusion words).
#[cfg_attr(not(feature = "jit"), allow(dead_code))]
pub(crate) const HEAP_JIT_CARD_TABLE: usize =
    std::mem::offset_of!(super::TaggedHeap, card_table_base_0);

const _: () = {
    let offsets = [
        std::mem::offset_of!(super::TaggedHeap, card_table_base_0),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_1),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_2),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_3),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_4),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_5),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_6),
        std::mem::offset_of!(super::TaggedHeap, card_table_base_7),
    ];
    let mut byte = 0;
    while byte < offsets.len() {
        assert!(offsets[byte] == HEAP_JIT_CARD_TABLE + byte);
        byte += 1;
    }
    // The base follows the exclusion words, so no older offset moved.
    assert!(HEAP_JIT_CARD_TABLE == super::HEAP_JIT_EXCLUSION_LO + 40);
};

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

    /// The table's fixed address, which compiled cons stores index.
    pub(crate) fn base_addr(&self) -> usize {
        self.bytes.as_ptr() as usize
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

    /// The 128 card bytes of the 64 KiB chunk at CHUNK_BASE, or `None` when
    /// every one is clean. They are contiguous in the table, so a clean
    /// chunk costs sixteen word loads folded by OR and nothing is copied.
    /// Collector side, with every mutator stopped.
    #[inline]
    pub(crate) fn dirty_chunk(&self, chunk_base: usize) -> Option<ChunkCards> {
        debug_assert_eq!(chunk_base % CHUNK_BYTES, 0);
        // SAFETY: a chunk's cards are CHUNK_CARDS consecutive in-table bytes
        // starting at a CHUNK_CARDS-aligned index, so the words are aligned
        // and in bounds; every mutator is stopped, so no store races.
        let words = unsafe {
            self.bytes
                .as_ptr()
                .add(card_index(chunk_base))
                .cast::<u64>()
        };
        let word = |index: usize| unsafe { words.add(index).read() };
        if (0..CHUNK_CARD_WORDS).fold(0, |any, index| any | word(index)) == 0 {
            return None;
        }
        Some(ChunkCards(std::array::from_fn(word)))
    }

    /// Clean every card of the 64 KiB chunk at CHUNK_BASE when one is
    /// dirty. Collector side, with every mutator stopped.
    #[inline]
    pub(crate) fn clear_chunk(&self, chunk_base: usize) {
        if self.dirty_chunk(chunk_base).is_none() {
            return;
        }
        // SAFETY: as in `dirty_chunk`; no mutator runs, so no store races.
        let words = unsafe {
            self.bytes
                .as_ptr()
                .add(card_index(chunk_base))
                .cast::<u64>()
        };
        for index in 0..CHUNK_CARD_WORDS {
            unsafe { words.add(index).write(0) };
        }
    }

    /// Collector side, with every mutator stopped.
    #[inline]
    pub(crate) fn clear(&self, owner_addr: usize) {
        self.byte(card_index(owner_addr))
            .store(CARD_CLEAN, Ordering::Relaxed);
    }
}

/// A copy of one chunk's card bytes with at least one dirty card.
#[derive(Clone, Copy)]
pub(crate) struct ChunkCards([u64; CHUNK_CARD_WORDS]);

impl ChunkCards {
    /// The byte offset from the chunk base of every dirty card, in address
    /// order; clean words are skipped eight cards at a time.
    #[inline]
    pub(crate) fn dirty_offsets(self) -> impl Iterator<Item = usize> {
        self.0
            .into_iter()
            .enumerate()
            .flat_map(|(word_index, word)| {
                // Byte k of the word is card 8 * word_index + k in memory order.
                let mut bytes = u64::from_le(word);
                std::iter::from_fn(move || {
                    if bytes == 0 {
                        return None;
                    }
                    let byte = bytes.trailing_zeros() as usize / 8;
                    bytes &= !(0xff << (byte * 8));
                    Some((word_index * size_of::<u64>() + byte) << CARD_SHIFT)
                })
            })
    }
}

impl Drop for CardTable {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with the same layout.
        unsafe { alloc::dealloc(self.bytes.as_ptr().cast(), Self::layout()) };
    }
}
