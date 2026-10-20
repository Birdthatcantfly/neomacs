# C1 qualification — standalone models only

Base: `fa5aa55e2b8ed9e11e996602c571fdcf103a2747`, branch
`v10x/peer-p3sym`. Only this independent test workspace is added; no
production source, root Cargo manifest/lockfile, dependency feature, engine
layout or shared authority helper changes. The approved design and review
addendum remain in the lane's ignored `tmp/peer-p3sym/design.md`.

**27/27 cases pass at preemption bound 2 and again at bound 3**, including
eight expected-failure controls in each run. Eight small good cases are
exhaustive in both runs; the other nineteen explicitly use the reported
bound. No duration/permutation limit, no truncated or skipped case.

| Run | Test-binary summaries (authority / epoch / keeper / publication) | Entered executions | Completed executions | Required failing witnesses |
| --- | --- | ---: | ---: | ---: |
| Bound 2 | 6/6, 6/6, 8/8, 7/7 | 1,388 | 1,380 | 8 |
| Bound 3 | 6/6, 6/6, 8/8, 7/7 | 5,895 | 5,887 | 8 |

The eight execution-count gaps are intentional: each broken control stops
on its first exact required witness. All nineteen good cases completed
every entered execution. Negative controls do not exhaust their failing
variants. The helper prints receipts even when Loom retains a failing
callback during unwind; host counters add no modeled synchronization.

## Per-case receipts

Each numeric pair is entered/completed. `all` means no preemption bound.

| Case | Bound | Run 2 | Run 3 |
| --- | --- | ---: | ---: |
| reader_count_saturation_refuses_without_wrapping | all | 1/1 | 1/1 |
| reader_not_retrying_across_writer_is_rejected | 2 / 3 | 1/0 | 1/0 |
| reader_retries_after_writer_release | all | 16/16 | 16/16 |
| readers_overlap_and_writer_drains | 2 / 3 | 11/11 | 15/15 |
| symbol_cell_copy_vs_writer | 2 / 3 | 803/803 | 4,334/4,334 |
| writer_bypassing_drain_races_checked_symbol_cell | 2 / 3 | 3/2 | 3/2 |
| active_old_function_root_survives_then_reclaims | 2 / 3 | 11/11 | 15/15 |
| concurrent_call_revalidates_complete_target_and_epoch | 2 / 3 | 64/64 | 136/136 |
| different_symbol_writers_cannot_lose_epoch_increment | 2 / 3 | 40/40 | 88/88 |
| missing_activation_root_has_witness | 2 / 3 | 1/0 | 1/0 |
| missing_epoch_bump_accepts_stale_call | 2 / 3 | 1/0 | 1/0 |
| same_definition_does_not_bump_epoch | all | 1/1 | 1/1 |
| completed_commit_remains_resumable | all | 1/1 | 1/1 |
| contender_can_ack_stop_outside_failed_writer_attempt | 2 / 3 | 11/11 | 15/15 |
| keeper_ignoring_word_zero_has_witness | 2 / 3 | 1/0 | 1/0 |
| keeper_rejects_ack_with_live_word | all | 1/1 | 1/1 |
| keeper_waits_for_ack_after_authority_release | 2 / 3 | 6/6 | 6/6 |
| partial_commit_poison_prevents_trace_and_resume | 2 / 3 | 19/19 | 28/28 |
| partial_commit_without_poison_has_witness | 2 / 3 | 1/0 | 1/0 |
| zero_word_without_all_acknowledgements_is_not_stop_authority | all | 1/1 | 1/1 |
| arms_before_name_release_publishes_complete_slot | all | 6/6 | 6/6 |
| different_names_publish_distinct_ids_and_count | 2 / 3 | 47/47 | 108/108 |
| publication_before_arms_has_witness | 2 / 3 | 3/2 | 3/2 |
| same_name_interns_have_one_winner_and_soft_never_sees_half | 2 / 3 | 198/198 | 900/900 |
| soft_miss_does_not_allocate_or_publish | all | 75/75 | 75/75 |
| spine_growth_is_exclusive_with_readers | 2 / 3 | 64/64 | 136/136 |
| unguarded_spine_growth_retires_live_reader_generation | 2 / 3 | 1/0 | 1/0 |

## Exact witness strings

Every string below appeared in both final logs. No arbitrary panic, spin
limit, timeout, branch-limit failure or failed unwrap can satisfy a control.

```text
reader admitted across WRITER without retry
Causality violation: Concurrent read and write accesses to `UnsafeCell`.
name published before symbol arms
spine retired while reader retained generation
call accepted stale target at unchanged epoch
active old function reclaimed after fset
keeper traced under nonzero symbol authority
unpoisoned partial commit reached keeper
```

## Gates and provenance

| Gate | Result |
| --- | --- |
| Model cargo test --locked --tests at bounds 2 and 3, one test thread | PASS, 27/27 each |
| Standalone cargo fmt --manifest-path … -- --check | PASS |
| Standalone cargo check --locked --manifest-path … --all-targets | PASS, no warnings |
| Root cargo fmt --all -- --check | PASS |
| Root cargo check --workspace --all-targets | PASS, existing workspace warnings retained |
| Source scope audit / git diff --check | PASS; only this new model package |

All Cargo commands use `taskset -c 0-3,14-31 nix develop -c`, with
`unset RUST_LOG` **inside** the Nix command shell and a lane-local TMPDIR.
Targets are `/home/exec/nvme-rust-target/v10x-peer-p3sym` (workspace) and
its `loom` subdirectory (models). Run recipe and abstraction limits are in
[README.md](README.md). No nextest run is claimed; the standalone Cargo
test result summaries were parsed and checked for nonzero expected counts.

Local raw evidence is under worktree `tmp/peer-p3sym/c1/` (ignored, retained).
SHA256 digests:

| Log | SHA256 |
| --- | --- |
| loom-bound2-final.log | `956d4fd391e1d0dd1c9b9baed73a9c78de3ee2c4761a9674247228ac4a3c2014` |
| loom-bound3-final.log | `152b0287ce2cafdbddba78ef6b2cf1e7f6a798b0eb1e046484cab3e1d1ddd27c` |
| model-final-gates.log | `c7df9f5332b404d281d0c81868f1ae24b295f15b4ba4c989e8055beee9e9a205` |
| workspace-final-gates.log | `dc0b515d42b34913bc3eb2573e128bc96b278f99f7a02c86c337b629aac417bf` |

Model input digest:
`3028a2111d826d71bf821739fd1bf32da5aecfae17196ba83413c6605d12ac9a`.
It covers eight inputs: Cargo.toml, Cargo.lock and all six tests/**/*.rs,
sorted by package-relative path; hash each relative path, NUL, bytes, NUL.
Documentation is excluded to avoid a self-referential evidence digest.

Earlier exploratory logs are retained, not passing receipts:

- `loom-bound2.log`: a scripted fault-injected writer incorrectly unwrapped
  a Busy acquisition when the probing reader acquired first. Fixed by
  explicitly sequencing that writer's first acquisition; the next-owner
  probe still races poison/release. This was a harness scheduling error.
- `loom-bound2-r2.log`: same-name intern model assumed at least one initial
  writer attempt must commit. Soft can hold a reader across both attempts;
  both Busy responses are legal. Fixed by retrying after join and retaining
  the exactly-once member/identity/name-object assertions.
- `loom-bound2-r3.log`: first complete passing suite, before replacing the
  explicit poison store with the final RAII unfinished-commit guard.
  Final receipts above cover that guard and both exploration bounds.

No GNU oracle, production architecture/compile-contract runner, Miri,
TSan, arm64 run, fat census, board or microbenchmark was run. Those are
engine-stage gates. Structural dormant impact in C1 is zero: the added
workspace is not an engine build input, and all production files and root
manifests are unchanged. This is not a measured census or a performance claim.

## Handoff

C1 is complete; **C2 remains blocked on the coordinator's upstream landing
signal**, not on model results. No open C1 design question. The b135 and
proof-2 correction, approved exclusions, single-World process fence,
reviewed-helper reuse rule and first-class reader-scaling/writer-drain rows
are recorded in README and the design addendum. C7 must name BRAVO-style
per-owner indicators, sharding or per-symbol authority as the follow-up lever.
Do not infer whole-engine parallel safety, public thread admission or C2
authorization from these bounded models. Nothing was pushed.
