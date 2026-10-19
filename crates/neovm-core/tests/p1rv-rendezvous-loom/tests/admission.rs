//! Turn-3 lock-free slice admission model (tmp/peer-constail/admission-design.md
//! sections 2, 3 and 7): an owner-written admission word per entry paired
//! against a read-mostly registry phase word through Dekker fences. No real
//! heap, gate implementation, VM or unsafe code.
//!
//! Modeled pieces: the owner fast path (admission store, SeqCst fence, phase
//! load, cold backout), the exit path (exit store, fence, phase load, ACK
//! checkpoint), the authority side (phase publication under the control
//! mutex, fence-then-scan before acting), the catalog transition window and
//! poison. The stop path's ACK accounting stays mode-based under the control
//! mutex exactly like production's gate-held `verify_stopped`.
//!
//! Stop-path honesty note: the stop is self-healing under every fault here,
//! because the keeper's mode/ACK accounting gates the trace and every owner
//! actor ends in a gated `settle` (production: the driver parks under the
//! gate between slice batches, and retirement is itself a gated protocol
//! step the keeper accounts). A faulted owner that misses the published stop
//! at every unfenced load therefore still ACKs from the gated path, so the
//! stop completes and no stop-path invariant can fire. The scan the protocol
//! *acts* on is the catalog transition's, so the negative controls witness
//! the Dekker pair there.
//!
//! Dekker-pair instrumentation: the phase word carries a transition counter,
//! bumped by the closing publish of every transition window. An admissible
//! word with counter zero is exactly the pre-transition initial word, so the
//! owner can report by value whether its fenced load acted on a word that
//! predates the transition, and the scan reports whether it observed the
//! owner's admission. The witnessed violation is both at once: the authority
//! acted quiescent while the owner's load still read the pre-transition
//! phase — the two-sided store->load reorder. With both fences this pair is
//! impossible (loom's SC-fence causality merge enforces it); removing either
//! fence or the scan admits it. Racing actors are joined only after the
//! observation, so no channel or join can supply an accidental
//! happens-before edge (visibility.rs house rule).
#![forbid(unsafe_code)]
mod support;

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};
use loom::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use loom::thread;
use support::negative as negative_control;

const SLICE_OVERLAPS_TRACE: &str = "ADMISSION_SLICE_OVERLAPS_ALL_STOPPED_EPOCH";
const PAIR_STALE: &str = "ADMISSION_PAIR_BOTH_SIDES_READ_STALE";
const OWNER_FENCE_WITNESS: &str = "ADMISSION_OWNER_FENCE_REORDER_MISSES_LIVE_SLICE";
const UNFENCED_SCAN_WITNESS: &str = "ADMISSION_UNFENCED_SCAN_MISSES_LIVE_SLICE";
const SKIPPED_SCAN_WITNESS: &str = "ADMISSION_SKIPPED_SCAN_MISSES_LIVE_SLICE";

const TAG_IDLE: usize = 0;
const TAG_STOPPING: usize = 1;
const TAG_TRACING: usize = 2;
const TAG_DRAINING: usize = 3;
const BLOCKED_BIT: usize = 1 << 4;

/// Phase word: bits 0..4 tag, bit 4 catalog-blocked, bits 8..16 epoch,
/// bits 16.. transition counter.
fn pack(tag: usize, blocked: bool, epoch: usize, transitions: usize) -> usize {
    (transitions << 16)
        | (epoch << 8)
        | if blocked { BLOCKED_BIT } else { 0 }
        | tag
}
fn tag_of(word: usize) -> usize {
    word & 0x0F
}
fn transition_count(word: usize) -> usize {
    word >> 16
}
fn admissible(word: usize) -> bool {
    tag_of(word) == TAG_IDLE && word & BLOCKED_BIT == 0
}

/// Admission word values: NEVER = not admitted, IN_SLICE packs the Idle
/// epoch observed at entry, EXITED marks a completed slice. EXITED lets a
/// scan distinguish an observed completed slice from a missed admission; the
/// packed epoch stays diagnostics-only (design section 5.1).
const NEVER: usize = 0;
const EXITED: usize = 2;
fn in_slice(epoch: usize) -> usize {
    (epoch << 1) | 1
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Running,
    Stopped,
    /// Gated retirement before any request: the actor left the registry, so
    /// the keeper's stop accounting counts it as satisfied (production
    /// retirement is itself a gated, accounted protocol step).
    Retired,
}

impl Mode {
    fn satisfied_for_stop(self) -> bool {
        matches!(self, Mode::Stopped | Mode::Retired)
    }
    fn satisfied_for_resume(self) -> bool {
        matches!(self, Mode::Running | Mode::Retired)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    None,
    /// Owner drops the SeqCst fences around its admission/phase word pair.
    MissingOwnerFence,
    /// Authority reads the admission words without the SeqCst fence.
    UnfencedScan,
    /// Authority never fences or reads the admission words.
    SkipScan,
}

/// What a protocol scan actually observed across the admission words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScanRead {
    /// Some admission word is IN_SLICE: a live slice.
    InSlice,
    /// Every word is EXITED: all owners observed and completed.
    Exited,
    /// Some word is still NEVER: the scan observed no admission from that
    /// owner (missed store, or the owner has not stored yet).
    Never,
    /// SkipScan: no read happened; the authority assumed quiescence.
    Unread,
}

impl ScanRead {
    fn quiescent(self) -> bool {
        !matches!(self, ScanRead::InSlice)
    }
    /// The authority acted without observing this owner's admission.
    fn missed(self) -> bool {
        matches!(self, ScanRead::Never | ScanRead::Unread)
    }
}

struct Control {
    tag: usize,
    epoch: usize,
    completed: usize,
    blocked: bool,
    transitions: usize,
    modes: Vec<Mode>,
    parked: usize,
    traces: usize,
}

struct Admission {
    control: Mutex<Control>,
    wake: Condvar,
    phase_word: AtomicUsize,
    admissions: Vec<AtomicUsize>,
    depth: AtomicUsize,
    poisoned: AtomicBool,
    fault: Fault,
}

/// One owner slice attempt (production `run_access` + `RunAccess::drop`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// Body ran to completion.
    Ran,
    /// A load read an inadmissible phase before the body; the driver
    /// retries (or parks gated); the admission path itself never parks.
    Retry,
    /// The exit load read Stopping; the owner ACKed and resumed.
    Acked,
}

/// What one slice attempt observed, for the Dekker-pair witnesses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SliceReport {
    outcome: Outcome,
    /// The fenced admission load read an admissible word from before every
    /// transition (counter zero) and the body ran on it.
    pre_transition_body: bool,
}

enum Transition {
    Refused,
    Performed { read: ScanRead },
}

impl Admission {
    fn new(owners: usize, fault: Fault) -> Arc<Self> {
        Arc::new(Self {
            control: Mutex::new(Control {
                tag: TAG_IDLE,
                epoch: 0,
                completed: 0,
                blocked: false,
                transitions: 0,
                modes: vec![Mode::Running; owners],
                parked: 0,
                traces: 0,
            }),
            wake: Condvar::new(),
            phase_word: AtomicUsize::new(pack(TAG_IDLE, false, 0, 0)),
            admissions: (0..owners).map(|_| AtomicUsize::new(NEVER)).collect(),
            depth: AtomicUsize::new(0),
            poisoned: AtomicBool::new(false),
            fault,
        })
    }

    /// Guard-drop publication: called while the control mutex is held, after
    /// every phase-relevant mutation (models RegistryGuard::drop/take_for_wait).
    fn publish(&self, c: &Control) {
        self.phase_word.store(
            pack(c.tag, c.blocked, c.epoch, c.transitions),
            Ordering::Release,
        );
    }

    /// The authority half of the Dekker pair: fence, then read every
    /// admission word. `epoch` is the gated epoch the caller acts under.
    fn scan(&self, epoch: usize) -> ScanRead {
        if self.fault == Fault::SkipScan {
            return ScanRead::Unread;
        }
        if self.fault != Fault::UnfencedScan {
            fence(Ordering::SeqCst);
        }
        let mut any_live = false;
        let mut any_never = false;
        for word in &self.admissions {
            let observed = word.load(Ordering::Acquire);
            if observed == NEVER {
                any_never = true;
            } else if observed != EXITED {
                // Diagnostics-only epoch accounting (design section 5.1): an
                // admitted slice carries an Idle epoch at or below the
                // registry's; the fence-ordered phase load is the authority.
                assert!(observed >> 1 <= epoch, "ADMISSION_EPOCH_INCONSISTENT");
                any_live = true;
            }
        }
        if any_live {
            ScanRead::InSlice
        } else if any_never {
            ScanRead::Never
        } else {
            ScanRead::Exited
        }
    }

    /// Owner fast path + exit.
    fn run_slice(&self, slot: usize, body: impl FnOnce()) -> SliceReport {
        let snapshot = self.phase_word.load(Ordering::Acquire);
        if self.poisoned.load(Ordering::Acquire) || !admissible(snapshot) {
            return SliceReport {
                outcome: Outcome::Retry,
                pre_transition_body: false,
            };
        }
        self.admissions[slot]
            .store(in_slice(snapshot >> 8), Ordering::Release);
        if self.fault != Fault::MissingOwnerFence {
            fence(Ordering::SeqCst);
        }
        let phase = self.phase_word.load(Ordering::Acquire);
        if self.poisoned.load(Ordering::Acquire) || !admissible(phase) {
            self.admissions[slot].store(NEVER, Ordering::Release);
            return SliceReport {
                outcome: Outcome::Retry,
                pre_transition_body: false,
            };
        }
        let report = SliceReport {
            outcome: Outcome::Ran,
            pre_transition_body: transition_count(phase) == 0,
        };
        self.depth.fetch_add(1, Ordering::Relaxed);
        body();
        // Depth steps precede the exit store: observing EXITED implies the
        // body already left the trace-overlap probe at zero.
        self.depth.fetch_sub(1, Ordering::Relaxed);
        self.admissions[slot].store(EXITED, Ordering::Release);
        if self.fault != Fault::MissingOwnerFence {
            fence(Ordering::SeqCst);
        }
        let exit = self.phase_word.load(Ordering::Acquire);
        if tag_of(exit) == TAG_STOPPING {
            // Bounded stop latency: the ACK happens at this slice's end. Any
            // other exit phase (including a still-set blocked bit) needs no
            // slow path: the slice is over and the next admission re-checks.
            let resumed = self.checkpoint(slot);
            return SliceReport {
                outcome: if resumed { Outcome::Acked } else { Outcome::Retry },
                pre_transition_body: report.pre_transition_body,
            };
        }
        report
    }

    /// The gated slow path reached with Stopping already published: ACK the
    /// epoch, wait for completion, resume. Unwinds without ACK on poison
    /// (design section 5.4).
    fn checkpoint(&self, slot: usize) -> bool {
        let mut c = self.control.lock().unwrap();
        c.parked += 1;
        while c.tag != TAG_STOPPING {
            if self.poisoned.load(Ordering::Acquire) {
                c.parked -= 1;
                return false;
            }
            c = self.wake.wait(c).unwrap();
        }
        c.parked -= 1;
        self.ack_and_resume(c, slot)
    }

    fn ack_and_resume(&self, mut c: MutexGuard<'_, Control>, slot: usize) -> bool {
        let epoch = c.epoch;
        c.modes[slot] = Mode::Stopped;
        self.wake.notify_all();
        loop {
            if self.poisoned.load(Ordering::Acquire) {
                return false;
            }
            if c.completed >= epoch {
                break;
            }
            c = self.wake.wait(c).unwrap();
        }
        c.modes[slot] = Mode::Running;
        if c.tag == TAG_DRAINING && c.modes.iter().all(|mode| mode.satisfied_for_resume()) {
            c.tag = TAG_IDLE;
            self.publish(&c);
        }
        self.wake.notify_all();
        true
    }

    /// The driver's gated idle park (production `wait_request`): block until
    /// a request exists, then ACK and resume. Unwinds on poison. `ready` runs
    /// after the park is registered, so a script can position the request
    /// after the registration without a check-wait gap.
    fn wait_request_after(&self, slot: usize, ready: impl FnOnce()) -> bool {
        {
            let mut c = self.control.lock().unwrap();
            c.parked += 1;
        }
        ready();
        let mut c = self.control.lock().unwrap();
        loop {
            if self.poisoned.load(Ordering::Acquire) {
                c.parked -= 1;
                return false;
            }
            match c.tag {
                TAG_STOPPING => break,
                _ => c = self.wake.wait(c).unwrap(),
            }
        }
        c.parked -= 1;
        self.ack_and_resume(c, slot)
    }

    fn wait_request(&self, slot: usize) -> bool {
        self.wait_request_after(slot, || {})
    }

    /// Actor termination: ACK any outstanding request, else retire gated.
    /// This gated path is the stop's liveness anchor: a faulted owner that
    /// missed the published stop at every unfenced load still ACKs here, and
    /// a pre-request exit is retired before the keeper freezes membership.
    fn settle(&self, slot: usize) -> bool {
        let mut c = self.control.lock().unwrap();
        if self.poisoned.load(Ordering::Acquire) {
            return false;
        }
        match c.tag {
            TAG_STOPPING => self.ack_and_resume(c, slot),
            TAG_IDLE => {
                c.modes[slot] = Mode::Retired;
                true
            }
            _ => {
                // Tracing/Draining without this owner's ACK cannot happen
                // under the keeper's accounting; retire defensively.
                c.modes[slot] = Mode::Retired;
                true
            }
        }
    }

    /// Authority: publish a request (keeper- or owner-driven; always under the
    /// control mutex, never per slice).
    fn request(&self) -> usize {
        let mut c = self.control.lock().unwrap();
        if c.tag == TAG_IDLE {
            c.epoch += 1;
            c.tag = TAG_STOPPING;
            self.publish(&c);
        }
        self.wake.notify_all();
        c.epoch
    }

    /// Keeper stop: request, mode/ACK accounting under the mutex, the fenced
    /// verify scan, the trace window asserting no live slice, then the resume
    /// drain.
    fn stop_all(&self) -> Result<usize, ()> {
        let epoch = self.request();
        let mut c = self.control.lock().unwrap();
        loop {
            if self.poisoned.load(Ordering::Acquire) {
                return Err(());
            }
            if c.tag == TAG_STOPPING
                && c.epoch == epoch
                && c.modes.iter().all(|mode| mode.satisfied_for_stop())
            {
                break;
            }
            c = self.wake.wait(c).unwrap();
        }
        drop(c);
        // Fence-then-scan verify outside the mutex, exactly like production's
        // post-critical-section `verify_stopped` head. Under the mode/ACK
        // accounting this cannot report a live slice in a correct run.
        let read = self.scan(epoch);
        assert!(read.quiescent(), "ADMISSION_VERIFY_SCAN_FOUND_LIVE_SLICE");
        assert_eq!(
            self.depth.load(Ordering::Acquire),
            0,
            "{SLICE_OVERLAPS_TRACE}"
        );
        let mut c = self.control.lock().unwrap();
        c.tag = TAG_TRACING;
        c.traces += 1;
        self.publish(&c);
        c.completed = c.epoch;
        c.tag = TAG_DRAINING;
        self.publish(&c);
        self.wake.notify_all();
        loop {
            if c.tag == TAG_IDLE {
                break;
            }
            if c.modes.iter().all(|mode| *mode == Mode::Retired) {
                // Every member retired before the request: no resumer exists
                // to discharge the drain, so the keeper completes the epoch
                // back to Idle itself (production: a zero-member stop has no
                // resume debt to discharge).
                c.tag = TAG_IDLE;
                self.publish(&c);
                break;
            }
            c = self.wake.wait(c).unwrap();
        }
        Ok(epoch)
    }

    /// Keeper-only catalog transition (design section 3.4): publish blocked
    /// under the mutex, fence, scan, and only then open the unsafe window.
    /// The closing publish bumps the transition counter, so a later
    /// admissible load can be attributed to a closed window by value.
    fn transition(&self) -> Transition {
        let mut c = self.control.lock().unwrap();
        if c.tag != TAG_IDLE || c.blocked {
            return Transition::Refused;
        }
        c.blocked = true;
        self.publish(&c);
        let epoch = c.epoch;
        drop(c);
        let read = self.scan(epoch);
        let mut c = self.control.lock().unwrap();
        c.blocked = false;
        c.transitions += 1;
        self.publish(&c);
        if read.quiescent() {
            Transition::Performed { read }
        } else {
            Transition::Refused
        }
    }

    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
        // The notify must share the predicate mutex: a waiter checks the
        // flag under the mutex and sleeps atomically with its release, so a
        // notify issued without the mutex can land in between and be lost.
        {
            let _park_gate = self.control.lock().unwrap();
            self.wake.notify_all();
        }
    }
}

#[test]
fn owner_slices_never_overlap_the_all_stopped_epoch() {
    let coverage = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = std::sync::Arc::clone(&coverage);
    support::check(4, move || {
        let model = Admission::new(2, Fault::None);
        let keeper = {
            let model = Arc::clone(&model);
            thread::spawn(move || model.stop_all().expect("epoch completes"))
        };
        let owners: Vec<_> = (0..2)
            .map(|slot| {
                let model = Arc::clone(&model);
                thread::spawn(move || {
                    let mut admitted = 0;
                    for _ in 0..3 {
                        if model.run_slice(slot, || {}).outcome == Outcome::Ran {
                            admitted += 1;
                        }
                    }
                    // Driver gated settle: ACKs any request the slices
                    // missed, or retires before the keeper freezes it.
                    model.settle(slot);
                    admitted
                })
            })
            .collect();
        let epoch = keeper.join().unwrap();
        let admitted: usize = owners.into_iter().map(|o| o.join().unwrap()).sum();
        if admitted > 0 {
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let c = model.control.lock().unwrap();
        assert_eq!(c.tag, TAG_IDLE);
        assert_eq!(c.completed, epoch);
        assert!(c.modes.iter().all(|mode| mode.satisfied_for_resume()));
        assert_eq!(c.traces, 1);
        // Nonvacuous same-thread reuse after the completed epoch.
        drop(c);
        assert_eq!(model.run_slice(0, || {}).outcome, Outcome::Ran);
    });
    assert!(
        coverage.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "no explored schedule admitted a slice"
    );
}

#[test]
fn parked_owner_loses_no_request_wake() {
    support::check(2, || {
        let model = Admission::new(1, Fault::None);
        let (parked_tx, parked_rx) = mpsc::channel();
        let owner = {
            let model = Arc::clone(&model);
            thread::spawn(move || {
                // The park flag and the tag share one mutex, and the request
                // notify happens under that same mutex, so the notify cannot
                // land in a check-wait gap; a late notify is re-checked
                // after the wait reacquires the mutex.
                model.wait_request_after(0, || {
                    parked_tx.send(()).unwrap();
                })
            })
        };
        parked_rx.recv().unwrap();
        assert_eq!(model.control.lock().unwrap().parked, 1);
        let epoch = model.stop_all().expect("stop completes");
        assert!(owner.join().unwrap(), "owner resumed after the epoch");
        let c = model.control.lock().unwrap();
        assert_eq!(c.completed, epoch);
        assert_eq!(c.parked, 0);
    });
}

#[test]
fn catalog_transition_refuses_inflight_slice_and_excludes_new_ones() {
    support::check(2, || {
        let model = Admission::new(1, Fault::None);
        let (inside_tx, inside_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let owner = {
            let model = Arc::clone(&model);
            thread::spawn(move || {
                model.run_slice(0, || {
                    inside_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
            })
        };
        inside_rx.recv().unwrap();
        // The admission word is live (the channel send is inside the body,
        // after the IN_SLICE store), so the scan must refuse.
        assert!(matches!(model.transition(), Transition::Refused));
        release_tx.send(()).unwrap();
        assert_eq!(owner.join().unwrap().outcome, Outcome::Ran);
        match model.transition() {
            Transition::Performed { read } => assert!(
                !read.missed(),
                "the drained scan must observe the completed slice"
            ),
            Transition::Refused => panic!("the drained registry must admit the transition"),
        }
        // Nonvacuous reuse after the complete transition.
        assert_eq!(model.run_slice(0, || {}).outcome, Outcome::Ran);
    });
}

#[test]
fn poison_reaches_parked_owner_and_refuses_further_admissions() {
    support::check(2, || {
        let model = Admission::new(1, Fault::None);
        let (parked_tx, parked_rx) = mpsc::channel();
        let owner = {
            let model = Arc::clone(&model);
            thread::spawn(move || {
                parked_tx.send(()).unwrap();
                model.wait_request(0)
            })
        };
        parked_rx.recv().unwrap();
        model.poison();
        assert!(!owner.join().unwrap(), "poisoned park must unwind");
        assert_eq!(
            model.run_slice(0, || {}).outcome,
            Outcome::Retry,
            "refuse poison"
        );
        assert!(model.stop_all().is_err(), "keeper must observe poison");
    });
}

/// The Dekker-pair racing schedule: one owner attempt races one authority
/// transition with no channel or join between the racing store and load.
/// Returns the owner's report and the transition result.
fn dekker_schedule(fault: Fault) -> (SliceReport, Transition) {
    let model = Admission::new(1, fault);
    let owner = {
        let model = Arc::clone(&model);
        thread::spawn(move || model.run_slice(0, || {}))
    };
    let transition = model.transition();
    // Join only after the observation (visibility.rs house rule).
    let report = owner.join().unwrap();
    (report, transition)
}

fn pair_failed(report: &SliceReport, transition: &Transition) -> bool {
    let missed = match transition {
        Transition::Performed { read } => read.missed(),
        Transition::Refused => false,
    };
    missed && report.pre_transition_body
}

#[test]
fn fenced_authority_scan_and_owner_admission_are_mutually_observable() {
    let bodies = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refusals = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let performed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (b, r, p) = (
        std::sync::Arc::clone(&bodies),
        std::sync::Arc::clone(&refusals),
        std::sync::Arc::clone(&performed),
    );
    support::check(2, move || {
        let (report, transition) = dekker_schedule(Fault::None);
        if report.outcome == Outcome::Ran {
            b.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        match transition {
            Transition::Refused => {
                r.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Transition::Performed { .. } => {
                p.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // The Dekker pair: the authority acted quiescent without observing
        // the admission only when the owner's fenced load did not act on a
        // pre-transition admissible word. At least one side must observe.
        assert!(!pair_failed(&report, &transition), "{PAIR_STALE}");
    });
    let (bodies, refusals, performed) = (
        bodies.load(std::sync::atomic::Ordering::Relaxed),
        refusals.load(std::sync::atomic::Ordering::Relaxed),
        performed.load(std::sync::atomic::Ordering::Relaxed),
    );
    assert!(bodies > 0, "no explored schedule ran a slice body");
    assert!(refusals > 0, "no explored schedule refused a live slice");
    assert!(performed > 0, "no explored schedule performed a transition");
}

fn pair_failure_control(fault: Fault, witness: &'static str) {
    let (report, transition) = dekker_schedule(fault);
    // Complete the finite actor DAG before the invariant panic (house rule).
    assert!(!pair_failed(&report, &transition), "{witness}");
}

#[test]
fn negative_missing_owner_fence_reorders_admission_past_the_scan() {
    negative_control(2, OWNER_FENCE_WITNESS, || {
        pair_failure_control(Fault::MissingOwnerFence, OWNER_FENCE_WITNESS);
    });
}

#[test]
fn negative_unfenced_authority_scan_misses_live_slice() {
    negative_control(2, UNFENCED_SCAN_WITNESS, || {
        pair_failure_control(Fault::UnfencedScan, UNFENCED_SCAN_WITNESS);
    });
}

#[test]
fn negative_skipped_authority_scan_misses_live_slice() {
    negative_control(2, SKIPPED_SCAN_WITNESS, || {
        pair_failure_control(Fault::SkipScan, SKIPPED_SCAN_WITNESS);
    });
}
