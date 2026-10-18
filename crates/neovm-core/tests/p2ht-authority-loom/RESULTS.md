# B1 exploration receipts

These results concern the independent model, not production GC. The run used
Loom 0.7.2, Rust 1.96.1, test optimization level 1, one libtest thread and
`--nocapture`, and the harness CPU/TMPDIR/target setup documented in
README.md. No `LOOM_MAX_PREEMPTIONS` was set: exploration is fully
exhaustive, not bounded to a preemption budget. A 512-branch path limit was
configured; no completed case hit it. Positive counts exhaust all schedules;
negative counts stop at the first exact witness. The harness rejects an
unrelated assertion, branch-limit panic or Loom deadlock as a substitute
witness (`support::negative` requires the exact panic string).

Fifteen tests passed and none failed: 6 authority, 2 growth, 4 guard and 3
revalidation. The full log is `tmp/peer-p2ht/logs/b1-run9-receipts.log` in
the lane worktree.

| Suite | Passed | Wall time |
| --- | ---: | ---: |
| authority | 6 / 6 | 26.36s |
| growth | 2 / 2 | 0.82s |
| guard | 4 / 4 | 43.53s |
| revalidate | 3 / 3 | 4.93s |

## Per-test iterations and witnesses

An em dash denotes a positive case (exhausted); a control's iterations are
witness-search iterations until the exact panic fired.

| Test | Exhaustive iterations | Required witness |
| --- | ---: | --- |
| `readers_overlap_without_serialising` | 6,171 | — |
| `writer_drains_readers_before_mutating` | 16,359 | — |
| `reader_retries_under_writer_then_sees_published_state` | 103 | — |
| `keeper_quiescent_after_all_actors_stop` | 419,895 | — |
| `writer_skipping_drain_observes_torn_entry` | 1,901 | torn entry observed |
| `word_layout_constants` | n/a (pure consts) | — |
| `revalidation_retries_until_epoch_stable` | 24,320 | — |
| `revalidation_converges_under_repeated_mutation` | 64,736 | — |
| `reader_skipping_revalidation_accepts_stale_entry` | 1 | stale answer accepted after callback |
| `nested_tests_two_owners_third_mutation_signals` | 1,005,480 | — |
| `guard_hold_unwinds_to_zero` | 1 | — |
| `own_mutation_inside_own_test_signals` | 1 | — |
| `bool_guard_lost_restore_misses_signal` | 1 | mutation inside active user test was not signaled |
| `growth_frees_old_backing_only_after_drain` | 16,359 | — |
| `growth_without_drain_frees_live_backing` | 601 | reader walked a freed backing |

Totals: 1,592,425 exhaustive positive iterations; 2,504 control iterations
across four witnesses.

## Sequencing notes (why two cases are shaped as they are)

Loom's DPOR multiplies yield-spin chains, so scripts that chain two or more
waits per actor, or that use a third spawned sequenced actor, exceed the
512-branch budget for minutes. Two cases were restructured for tractability
without weakening their contract:

- `nested_tests_two_owners_third_mutation_signals` — the third owner runs on
  the model's main thread and the two holders block on latches; both holders
  are provably inside their tests (depth 2) when the check runs, in every
  explored execution. 1,005,480 schedules exhausted in 43.53s.
- `revalidation_converges_under_repeated_mutation` — each of two mutation
  windows is opened and closed by latches and races exactly one post-callback
  epoch load, keeping the windows independent instead of multiplying; the
  free-running single-window race (writer released into the unbounded retry
  loop) is exhaustively covered by `revalidation_retries_until_epoch_stable`.

## Tested source identity

SHA-256 hashes bind these receipts to the model, harness and locked build
inputs. README/results prose is excluded.

| Input | SHA-256 |
| --- | --- |
| `Cargo.lock` | `23a84dce17615494d59dce9184af4d89ad30f0be0ceed699bae8ae6e6ee55ee7` |
| `Cargo.toml` | `b6bfc2d6a6e943c661eaf19e8f3043f3edbf0eea68668ba042dba3c4ee7e4802` |
| `tests/model/mod.rs` | `ac25978e7b8a5b361e066bb0e8967702201a21bf2a1df52bc9982b193aa49941` |
| `tests/authority.rs` | `7b2ad6ff4041774e6511f0d17a27b014381575ecefb9ff647f22c6404b7e0818` |
| `tests/growth.rs` | `3c81dbd3fb8b8771bc01887cb0b09b2a2e6103eef0a6631566518a070dc3340f` |
| `tests/guard.rs` | `3b98bf7cc690f005bfb5e9b0d060987188724c0b2b3504c146f977e88b25633f` |
| `tests/revalidate.rs` | `d041e83ea7e314e86a38a4705d7187bb3db1f90b41ef221b58efdf624715a944` |
| `tests/support/mod.rs` | `cf98d5cb83d0e753a9991352555fa8d5ac777b5434dfcb9638cee2ea4560bf67` |
