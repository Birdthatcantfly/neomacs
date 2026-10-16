# Root-batch slot ordering model

This is an independent test workspace. It adds no dependency, feature or code
to the production workspace and models no Lisp evaluator. Its manifest pins
Loom 0.7.2, num_enum 0.7.6 and static_assertions 1.1.0, matching the reviewed P7.4
standalone workspace. The lockfile reuses that exact transitive graph with only
the root package name changed.

The model corresponds to the Part A batch protocol and Part B in-place reuse in
`src/tagged/transport/root_batch.rs` and its common allocator in
`src/tagged/transport/root_table.rs`. Part A starts at
`a637317d7d45cfa3682662e202814bf7a30516b7`. Gate receipts bind the final production
and model sources; the correspondence below defines this model's scope.

| Operation | Modeled contract |
| --- | --- |
| Reserve | Confined Prepared endpoint, fresh generation/control, held exclusion |
| Finalize | Initialize every payload word; retain Reserved and confinement |
| Publish | Slot/table mutex, Release OnceLock installation, Release Live |
| Admit explicit reader | Slot/table mutex, Acquire Live, AcqRel increment active readers |
| Trace roots | Same mutex, Acquire Live or Closing; ignore terminal/unpublished slots |
| Close | Try the same mutex; Busy preserves ownership; otherwise Release Closing |
| Reader finish | Consume reader, Release active-reader decrement |
| Reader Drop | Release abandoned increment, then Release active-reader decrement |
| Try finish | Acquire readers first, then Acquire abandoned; both zero before Retired |
| Failed finish | Preserve Closing lease ownership and root visibility |
| Successful finish | Retired payload no longer traced; release retained exclusion |
| Cancel | Unpublished Reserved payload becomes Cancelled; release its retention |
| Abandon lease | Keep Live/Closing roots and the logical exclusion obligation |
| Ordinary slot reuse | Allocate a new generation and control; stale control remains separate |
| In-place `try_recycle` | Same table lock; close; reject readers/abandonment and nonunique control/payload; renew generation and clear roots |
| Recycle failure | Preserve the Live/Closing lease, root visibility and exclusion for retry |
| `ensure_capacity` | Grow only unique, empty Prepared storage; failed allocation preserves its buffer and reservation |

Each slot's payload has a traced heap-object word, a traced uninterned-symbol
word, and an immediate `nil` which contributes no root. A typed input oracle
computes the expected roots independently of slot-state decoding. Production
payload words are immutable host storage. The model uses Relaxed atomics for
these words, so a missing Release/Acquire edge produces an exact stale-word
assertion. An additional Release/Acquire initialization flag represents the
OnceLock edge without pretending to contain a payload pointer. This does not
model dereferencing plain payload memory, prove OnceLock itself, or establish
the absence of a production data race by itself. No unsafe code is used here.

The model's retained-epoch counter is observational. Live/Closing abandonment
leaves that logical obligation held; it does not reproduce the production Arc
leak/owner retention mechanism. The Slot's separate observational Epoch Arc
does not increment the count. Consequently, retaining a terminal slot's host
storage cannot itself prohibit future marking. The model checks mark-launch
eligibility only; it does not implement concurrent marking, sweep, collector
quiescence, heap matching, TLS exchange or the Part B registry.

The separate `reuse` model stores actual Loom Arcs for the table's control and
immutable payload. Scans borrow that payload under the table mutex, and explicit
readers clone both Arcs only after generation/phase validation under that mutex.
Successful recycle checks the same strong counts as production, removes the
consumed lease's control reference, and uses `Arc::get_mut` to clear payload and
renew the existing control. A three-thread case races recycle against reader
admission/finish and a synchronous scan. Loom Arc does not expose weak references;
production's weak-count checks need its transport tests. Host allocation failure
is injected before buffer replacement; the model does not emulate an allocator.

Prepared, Finalized, Reader and Exclusion endpoints are pinned as non-Send,
non-Sync, non-Clone and non-Copy. The rooted Lease is Send/Sync and cannot be
cloned or copied. Readers are created on the thread that uses them. Exploring
the inner counters independently of the outer Rust lease borrow allows more
executions than the safe public API; that borrow adds an independent restriction
which the compiler fixtures lock.

## Cases and controls

The twenty-one cases consist of sixteen positive cases and five
expected-failure controls. Positive protocol cases cover complete publication, cancellation
before/after finalize, close with an active reader, explicit finish racing close,
reader abandonment, lease abandonment, Busy retirement retaining ownership and
exclusion for retry, and fresh control/generation on ordinary slot reuse. Six
additional Part B cases cover in-place reuse races; stale-generation reader/key
rejection after renewal, including after publication; initialized capacity
growth and failed allocation; active readers and nonunique control/payload
observers retaining the lease; abandoned-reader retention; Busy recycle retry;
and generation exhaustion followed by explicit finish.

The separately named state-only positive experiment removes the publisher's
table-lock and OnceLock edges while retaining state Release/Acquire. Its two
negative controls weaken exactly one remaining state ordering. They establish
that the isolated state edge publishes the words, not that changing only that
ordering would break the current stronger production implementation.

The controls are:

- State-only Release publication becomes Relaxed.
- State-only Acquire observation becomes Relaxed.
- Retirement ignores active readers.
- Reader Drop counts as explicit finish by ignoring abandonment.
- Retirement loads abandonment before readers, accepting two unrelated zeroes.

The other three controls retain the full production publication layers and
change only their stated reader/retirement decision. Each negative case requires
its particular invariant assertion text. An unrelated
panic, branch-bound failure or deadlock cannot satisfy that expected failure.
The counter-order control specifically checks the edge from the Drop's earlier
abandonment increment through its Release final-reader decrement to the closer's
Acquire zero load; loading abandonment first loses that guarantee.

Part A cases use at most two threads and the Part B race uses three. All retain
a 256-branch safety bound, with no permutation, time or checkpoint truncation.
`LOOM_MAX_PREEMPTIONS` is read by Loom's Builder and preserved. Every case prints
`LOOM_ITERATIONS <test> <count>`; for expected-panic controls that count ends at
the required counterexample, while positive cases exhaust the configured bound.
There is no spin/retry liveness claim. This is a bounded model, not a complete
memory-model proof; Loom's coverage of Relaxed executions also has documented
limits. The table's zero-live fast exit, actual payload allocation/freeing,
production Arc retention, weak references and held-exclusion launch authority
require their production tests and type checks.

Run with the lane's owned target and pinned dependencies, in the authorized Nix
environment, with `RUSTFLAGS="--cfg loom"` and `LOOM_MAX_PREEMPTIONS=2`, then `3`.
The crate's `[workspace]` intentionally isolates its lockfile.
