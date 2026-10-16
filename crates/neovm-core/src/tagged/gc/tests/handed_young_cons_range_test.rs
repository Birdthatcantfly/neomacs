//! The actual store-path predicate agrees with independent ordered intervals.

use super::{CONS_BLOCK_ALIGN, CONS_CELLS_BYTES, handed_young_cons_range_contains};

const GRANULE: usize = CONS_BLOCK_ALIGN;
const MASK: usize = GRANULE - 1;

#[test]
fn handed_young_range_closed_rejects_endpoints_and_foreign_integers() {
    for addr in [0, 1, 15, 16, MASK, GRANULE, usize::MAX - 1, usize::MAX] {
        assert!(
            !handed_young_cons_range_contains(0, 0, addr),
            "closed range admitted {addr:#x}"
        );
    }
}

#[test]
fn handed_young_range_preserves_half_open_endpoints_and_full_coalesced_payload() {
    for base in [GRANULE, usize::MAX & !MASK] {
        for first_offset in [0u16, 16u16] {
            let first = base + usize::from(first_offset);
            let cur = base + CONS_CELLS_BYTES;
            assert!(handed_young_cons_range_contains(cur, first_offset, first));
            assert!(handed_young_cons_range_contains(
                cur,
                first_offset,
                first + 1
            ));
            // Past several former 128-cell grant boundaries, still young.
            assert!(handed_young_cons_range_contains(
                cur,
                first_offset,
                base + 4096
            ));
            assert!(handed_young_cons_range_contains(cur, first_offset, cur - 1));
            assert!(!handed_young_cons_range_contains(
                cur,
                first_offset,
                first - 1
            ));
            assert!(!handed_young_cons_range_contains(cur, first_offset, cur));
            assert!(!handed_young_cons_range_contains(
                cur,
                first_offset,
                cur + 1
            ));
            assert!(!handed_young_cons_range_contains(
                cur,
                first_offset,
                usize::MAX
            ));
        }
    }
}

#[test]
fn handed_young_range_rejects_foreign_granules_with_matching_low_bits() {
    for base in [0usize, GRANULE, usize::MAX & !MASK] {
        let first_offset = 16u16;
        let first = base + usize::from(first_offset);
        let cur = base + 128;
        assert!(handed_young_cons_range_contains(cur, first_offset, first));
        for foreign in [
            first.wrapping_sub(GRANULE),
            first.wrapping_add(GRANULE),
            cur.wrapping_sub(GRANULE),
            cur.wrapping_add(GRANULE),
            first.wrapping_sub(1),
            0,
            usize::MAX,
        ] {
            assert!(
                !handed_young_cons_range_contains(cur, first_offset, foreign),
                "foreign address {foreign:#x}, base {base:#x}"
            );
        }
    }
}

#[test]
fn handed_young_range_matches_independent_ordered_interval_at_integer_boundaries() {
    let mut bases = vec![0, GRANULE, usize::MAX & !MASK];
    for shift in [31u32, 32, 47, usize::BITS - 1] {
        if shift < usize::BITS {
            bases.push((1usize << shift) & !MASK);
        }
    }
    bases.sort_unstable();
    bases.dedup();
    for base in bases {
        for low_cur in [16usize, 32, 128, 2048, 4096, CONS_CELLS_BYTES] {
            for first_offset in [0u16, 16, 32, 128, 2048, 4096] {
                if usize::from(first_offset) > low_cur {
                    continue;
                }
                // Valid same-granule metadata, including empty intervals. The oracle
                // uses ordinary interval ordering, not either distance test.
                let first = base + usize::from(first_offset);
                let cur = base + low_cur;
                let expected_interval = first..cur;
                for addr in [
                    0,
                    1,
                    usize::MAX - 1,
                    usize::MAX,
                    first,
                    first + 1,
                    first.wrapping_sub(1),
                    first + (cur - first) / 2,
                    cur - 1,
                    cur,
                    cur + 1,
                    first.wrapping_sub(GRANULE),
                    first.wrapping_add(GRANULE),
                ] {
                    assert_eq!(
                        handed_young_cons_range_contains(cur, first_offset, addr),
                        expected_interval.contains(&addr),
                        "cur={cur:#x}, offset={first_offset}, addr={addr:#x}"
                    );
                }
            }
        }
    }
}
