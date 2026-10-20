//! TODO(proof-2 integration): deduplicate with table_authority.rs once landed.
//! Copy of its reviewed WRITER/count/CAS and release protocol. These adapters
//! attempt once instead of spinning: Busy returns outside every loan, so the
//! caller can close its RunAccess and service a stop. Saturation refuses.
use std::sync::atomic::{AtomicU64, Ordering};

const WRITER: u64 = 1 << 63;
const COUNT_MASK: u64 = (1 << 47) - 1;

pub(super) fn reader_enter(word: &AtomicU64) -> bool {
    let current = word.load(Ordering::Acquire);
    if current & !COUNT_MASK != 0 || current == COUNT_MASK {
        return false;
    }
    word.compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}
pub(super) fn reader_exit(word: &AtomicU64) {
    let was = word.fetch_sub(1, Ordering::Release);
    debug_assert!(was & COUNT_MASK > 0 && was & WRITER == 0);
}
pub(super) fn writer_enter(word: &AtomicU64) -> bool {
    word.compare_exchange(0, WRITER, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}
pub(super) fn writer_exit(word: &AtomicU64) {
    word.store(0, Ordering::Release);
}
pub(super) fn quiescent(word: &AtomicU64) -> bool {
    word.load(Ordering::Acquire) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn p3sym_word_refuses_writer_saturation_and_reserved_bits() {
        for occupied in [WRITER, COUNT_MASK, 1 << 50] {
            let word = AtomicU64::new(occupied);
            assert!(!reader_enter(&word));
            assert!(!writer_enter(&word));
            assert_eq!(word.load(Ordering::Relaxed), occupied);
        }
    }
}
