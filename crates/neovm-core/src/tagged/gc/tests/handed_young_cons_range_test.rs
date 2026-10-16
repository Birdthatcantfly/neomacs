//! The actual biased store-path predicate agrees with ordered owner intervals.

use super::{CONS_BLOCK_ALIGN, CONS_CELLS_BYTES, RegionBook, handed_young_cons_range_contains};
use crate::tagged::gc::alloc_region::RegionColor;
use crate::tagged::value::TAG_MASK;

const GRANULE: usize = CONS_BLOCK_ALIGN;
const MASK: usize = GRANULE - 1;
const OWNER_STEP: usize = TAG_MASK + 1;

fn bias(first: usize, color: RegionColor) -> usize {
    let mut book = RegionBook::new();
    book.set_cons_first_and_color(first, color);
    book.cons_young_bias
}

#[test]
fn handed_young_range_closed_rejects_endpoints_and_foreign_integers() {
    for color in [RegionColor::White, RegionColor::Black] {
        let mut book = RegionBook::new();
        book.set_cons_first_and_color(GRANULE, color);
        book.clear_cons_first();
        assert_eq!(book.cons_first(), 0);
        for addr in [
            0,
            OWNER_STEP,
            16,
            MASK & !TAG_MASK,
            GRANULE,
            usize::MAX & !TAG_MASK,
        ] {
            assert!(
                !handed_young_cons_range_contains(0, book.cons_young_bias, addr),
                "closed {color:?} range admitted {addr:#x}"
            );
        }
    }
}

#[test]
fn handed_young_range_preserves_half_open_endpoints_and_full_coalesced_payload() {
    for base in [GRANULE, usize::MAX & !MASK] {
        for first_offset in [0usize, 16] {
            let first = base + first_offset;
            let cur = base + CONS_CELLS_BYTES;
            for color in [RegionColor::White, RegionColor::Black] {
                let word = bias(first, color);
                for addr in [first, first + OWNER_STEP, base + 4096, cur - OWNER_STEP] {
                    assert!(handed_young_cons_range_contains(cur, word, addr));
                }
                for addr in [
                    first - OWNER_STEP,
                    cur,
                    cur + OWNER_STEP,
                    usize::MAX & !TAG_MASK,
                ] {
                    assert!(!handed_young_cons_range_contains(cur, word, addr));
                }
            }
        }
    }
}

#[test]
fn handed_young_range_rejects_foreign_granules_with_matching_low_bits() {
    for base in [0usize, GRANULE, usize::MAX & !MASK] {
        let first = base + 16;
        let cur = base + 128;
        for color in [RegionColor::White, RegionColor::Black] {
            let word = bias(first, color);
            assert!(handed_young_cons_range_contains(cur, word, first));
            for foreign in [
                first.wrapping_sub(GRANULE),
                first.wrapping_add(GRANULE),
                cur.wrapping_sub(GRANULE),
                cur.wrapping_add(GRANULE),
                first - OWNER_STEP,
                0,
                usize::MAX & !TAG_MASK,
            ] {
                assert!(
                    !handed_young_cons_range_contains(cur, word, foreign),
                    "foreign address {foreign:#x}, base {base:#x}, color {color:?}"
                );
            }
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
            for first_offset in [0usize, 16, 32, 128, 2048, 4096] {
                if first_offset > low_cur {
                    continue;
                }
                let first = base + first_offset;
                let cur = base + low_cur;
                let expected_interval = first..cur;
                // The production caller clears TAG_MASK. Black bias can admit
                // first-1 as a raw byte integer; it is never a valid owner.
                for color in [RegionColor::White, RegionColor::Black] {
                    let word = bias(first, color);
                    for addr in [
                        0,
                        OWNER_STEP,
                        usize::MAX & !TAG_MASK,
                        first,
                        first + OWNER_STEP,
                        first.wrapping_sub(OWNER_STEP),
                        (first + (cur - first) / 2) & !TAG_MASK,
                        cur - OWNER_STEP,
                        cur,
                        cur + OWNER_STEP,
                        first.wrapping_sub(GRANULE),
                        first.wrapping_add(GRANULE),
                    ] {
                        assert_eq!(addr & TAG_MASK, 0);
                        assert_eq!(
                            handed_young_cons_range_contains(cur, word, addr),
                            expected_interval.contains(&addr),
                            "cur={cur:#x}, first={first:#x}, addr={addr:#x}, color={color:?}"
                        );
                    }
                }
            }
        }
    }
}
