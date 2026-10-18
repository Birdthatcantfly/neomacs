//! Proof 2 stage B2: authority-word layout pins and zero-on-copy fixtures.
//!
//! The layout numbers below are pinned, not derived, on purpose: the JIT
//! reads `switch_epoch` at a compile-time constant offset in generated code
//! ([`super::switch_plan::SWITCH_EPOCH_OFFSET`]), so an accidental field
//! reorder must fail here rather than misread tables in optimized builds.
//! The base (pre-B2, 19dba56fa3) was HashTableObj 288 / LispHashTable 264 /
//! HashTableStorage 192 with the epoch at 240; B2 appends the authority
//! word after `switch_epoch` and swaps `mutable: bool` for the equal-size
//! `test_depth: AtomicU8`, and the epoch offset must not move.

use super::*;

/// The authority word's reserved bits, as in `table_authority.rs`.
const WRITER: u64 = 1 << 63;

#[test]
fn p2ht_layout_pins_the_jit_read_offset() {
    use std::mem::{offset_of, size_of};

    // Sizes: the shell grew by exactly the 8-byte word and the 1-byte swap
    // stayed 1 byte (alignment 1) inside existing padding.
    assert_eq!(size_of::<HashTableObj>(), 296);
    assert_eq!(size_of::<LispHashTable>(), 272);
    assert_eq!(size_of::<HashTableStorage>(), 200);

    // THE invariant: the JIT-read epoch offset is byte-for-byte unchanged
    // from the pre-B2 layout (240 on this target), and the const the JIT
    // bakes in still names the real field.
    let epoch_offset = offset_of!(HashTableObj, table.data.switch_epoch);
    assert_eq!(epoch_offset, 240);
    assert_eq!(switch_plan::SWITCH_EPOCH_OFFSET, epoch_offset);

    // The authority word sits immediately after `switch_epoch` and is the
    // LAST field of storage: appending is what keeps every earlier offset
    // stable.
    let authority_offset = offset_of!(HashTableObj, table.data.authority);
    assert_eq!(authority_offset, epoch_offset + size_of::<u64>());
    assert_eq!(
        offset_of!(HashTableStorage, authority),
        size_of::<HashTableStorage>() - size_of::<u64>()
    );

    // R6 size-class check: `Box::new` hands the global allocator
    // `size_of::<HashTableObj>()` bytes. 288 (base) and 296 (B2) both fit
    // the usable payload of the same glibc 304-byte chunk class (304 - 8
    // chunk header = 296), so the alloc-site class and `malloc_usable_size`
    // the engine may see are unchanged. Only a further 1-byte growth would
    // cross it; the pinned size above catches that too.
    assert!(size_of::<HashTableObj>() <= 296);
}

#[test]
fn p2ht_clone_zeroes_authority_and_keeps_the_depth_count() {
    let source = LispHashTable::new_with_options(HashTableTest::Eq, 0, None, 1.5, 0.8125);
    source
        .test_depth
        .store(2, std::sync::atomic::Ordering::Relaxed);
    source
        .data
        .authority
        .store(WRITER | 7, std::sync::atomic::Ordering::Relaxed);

    // Storage Clone is the zero-on-copy half (Rev R1 §2.1): a copy starts
    // quiescent, never inheriting an in-flight writer or reader count.
    let cloned_storage = source.data.clone();
    assert_eq!(
        cloned_storage
            .authority
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a copied table never inherits live authority"
    );

    // The shell Clone keeps the nesting count, matching the pre-B2 derive
    // that copied `mutable` (parity for any direct Rust consumer); the
    // Lisp-visible copy site resets it instead — the copy-inside-test
    // fixture in `hashtab::tests` pins that path.
    let cloned_shell = source.clone();
    assert_eq!(
        cloned_shell
            .test_depth
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    assert_eq!(
        cloned_shell
            .data
            .authority
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(cloned_shell.test, HashTableTest::Eq);
}

#[test]
fn p2ht_fresh_constructions_are_quiescent() {
    // Every constructor the engine reaches a new table through starts the
    // word at zero: with_storage (make-hash-table, pdump restore), Default
    // (empty storage), and with_index_and_slots (rebuilds).
    let fresh = LispHashTable::new_with_options(HashTableTest::Eq, 4, None, 1.5, 0.8125);
    assert_eq!(
        fresh
            .data
            .authority
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert_eq!(
        fresh.test_depth.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let default_storage = HashTableStorage::default();
    assert_eq!(
        default_storage
            .authority
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}
