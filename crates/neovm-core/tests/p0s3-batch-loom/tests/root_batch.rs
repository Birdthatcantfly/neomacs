//! An independent model of the root-table batch protocol, not VM code.
//!
//! Atomic model words make a missing publication edge an exact stale-payload
//! assertion rather than hiding it behind a model-only mutex. The production
//! payload is immutable host storage, not these atomics. No raw Lisp pointers
//! or unsafe operations are used here.

#![forbid(unsafe_code)]

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use loom::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex, mpsc};
use loom::thread;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use static_assertions::{assert_impl_all, assert_not_impl_any};

#[derive(Clone, Copy, Debug, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum Phase {
    Reserved,
    Live,
    Closing,
    Retired,
    Cancelled,
}

assert_impl_all!(Phase: Send, Sync, Copy, fmt::Debug);
const _: () = {
    assert!(std::mem::size_of::<Phase>() == std::mem::size_of::<u8>());
    assert!(std::mem::align_of::<Phase>() == std::mem::align_of::<u8>());
};

mod sealed {
    pub trait Protocol {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationLayer {
    /// Production also installs a OnceLock payload while holding the table
    /// lock. Both supply synchronization independent of the state word.
    TableAndOnce,
    /// A separate experiment isolates just the state publication edge.
    StateOnly,
}

trait Protocol: sealed::Protocol + Send + Sync + fmt::Debug + 'static {
    const PUBLICATION_LAYER: PublicationLayer = PublicationLayer::TableAndOnce;
    const PUBLISH: Ordering = Ordering::Release;
    const OBSERVE: Ordering = Ordering::Acquire;
    const CHECK_ACTIVE_READERS: bool = true;
    const CHECK_ABANDONED_READERS: bool = true;
    const ABANDONMENT_FIRST: bool = false;
}

macro_rules! protocol {
    ($name:ident $(, $field:ident = $value:expr)*) => {
        #[derive(Debug)]
        struct $name;
        impl sealed::Protocol for $name {}
        impl Protocol for $name {
            $(const $field: protocol!(@type $field) = $value;)*
        }
        assert_impl_all!($name: Send, Sync, fmt::Debug);
    };
    (@type PUBLISH) => { Ordering };
    (@type OBSERVE) => { Ordering };
    (@type PUBLICATION_LAYER) => { PublicationLayer };
    (@type CHECK_ACTIVE_READERS) => { bool };
    (@type CHECK_ABANDONED_READERS) => { bool };
    (@type ABANDONMENT_FIRST) => { bool };
}

protocol!(Production);
protocol!(
    StatePublicationOnly,
    PUBLICATION_LAYER = PublicationLayer::StateOnly
);
protocol!(
    RelaxedPublication,
    PUBLICATION_LAYER = PublicationLayer::StateOnly,
    PUBLISH = Ordering::Relaxed
);
protocol!(
    RelaxedObservation,
    PUBLICATION_LAYER = PublicationLayer::StateOnly,
    OBSERVE = Ordering::Relaxed
);
protocol!(PrematureRetirement, CHECK_ACTIVE_READERS = false);
protocol!(DropCountsAsFinish, CHECK_ABANDONED_READERS = false);
protocol!(WrongCounterOrder, ABANDONMENT_FIRST = true);

/// The independent semantic oracle includes an uninterned symbol. Filtering
/// only heap-object pointers would incorrectly omit that root.
#[derive(Clone, Copy, Debug)]
enum InputValue {
    HeapObject,
    UninternedSymbol,
    Nil,
}

impl InputValue {
    fn root_word(self) -> Option<usize> {
        match self {
            Self::HeapObject => Some(11),
            Self::UninternedSymbol => Some(23),
            Self::Nil => None,
        }
    }
}

const INPUT: [InputValue; 3] = [
    InputValue::HeapObject,
    InputValue::UninternedSymbol,
    InputValue::Nil,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Generation {
    First,
    Second,
}

/// Instrument the held-exclusion obligation independently of the slot's
/// storage Arc. A retained slot/node is not itself an exclusion owner.
#[derive(Debug)]
struct Epoch {
    held: AtomicUsize,
    admission: Mutex<()>,
}

assert_impl_all!(Epoch: Send, Sync, fmt::Debug);
assert_not_impl_any!(Epoch: Clone, Copy);

impl Epoch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            held: AtomicUsize::new(0),
            admission: Mutex::new(()),
        })
    }

    fn may_launch(&self) -> bool {
        let _admission = self.admission.lock().unwrap();
        self.held.load(Ordering::Acquire) == 0
    }
}

#[derive(Debug)]
struct Exclusion {
    epoch: Arc<Epoch>,
    _thread: PhantomData<Rc<()>>,
}

assert_not_impl_any!(Exclusion: Send, Sync, Clone, Copy);
assert_impl_all!(Exclusion: fmt::Debug);

impl Exclusion {
    fn acquire(epoch: &Arc<Epoch>) -> Self {
        let _admission = epoch.admission.lock().unwrap();
        epoch.held.fetch_add(1, Ordering::Release);
        Self {
            epoch: Arc::clone(epoch),
            _thread: PhantomData,
        }
    }
}

impl Drop for Exclusion {
    fn drop(&mut self) {
        self.epoch.held.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Debug)]
struct Slot<P> {
    phase: AtomicU8,
    /// The production slot/table lock serializes publication, explicit reader
    /// admission, close and retirement. State-only experiments omit this lock
    /// ONLY on publication, leaving the reader/retirement protocol unchanged.
    admission: Mutex<()>,
    readers: AtomicUsize,
    abandoned: AtomicUsize,
    words: [AtomicUsize; 3],
    /// Release/Acquire initialization models the OnceLock publication edge,
    /// without pretending the model contains a real payload pointer.
    payload_initialized: AtomicBool,
    generation: Generation,
    /// Observational only: this Arc does not increment `epoch.held`.
    epoch: Arc<Epoch>,
    _protocol: PhantomData<P>,
}

assert_impl_all!(Slot<Production>: Send, Sync, fmt::Debug);
assert_not_impl_any!(Slot<Production>: Clone, Copy);

impl<P: Protocol> Slot<P> {
    fn phase(&self, ordering: Ordering) -> Phase {
        Phase::try_from(self.phase.load(ordering)).expect("closed model phase")
    }

    fn store_phase(&self, phase: Phase, ordering: Ordering) {
        self.phase.store(phase.into(), ordering);
    }

    fn assert_words(&self) {
        if P::PUBLICATION_LAYER == PublicationLayer::TableAndOnce {
            assert!(
                self.payload_initialized.load(Ordering::Acquire),
                "accepted unpublished batch payload"
            );
        }
        for (word, expected) in self.words.iter().zip(INPUT) {
            assert_eq!(
                word.load(Ordering::Relaxed),
                expected.root_word().unwrap_or(0),
                "accepted unpublished batch payload"
            );
        }
    }

    /// The common GC root walk keeps Closing visible even though new explicit
    /// readers are forbidden. Its lock protects payload unlink/recycling.
    fn trace(&self) -> Vec<usize> {
        let _admission = self.admission.lock().unwrap();
        match self.phase(P::OBSERVE) {
            Phase::Live | Phase::Closing => {
                self.assert_words();
                self.words
                    .iter()
                    .map(|word| word.load(Ordering::Relaxed))
                    .filter(|word| *word != 0)
                    .collect()
            }
            Phase::Reserved | Phase::Cancelled | Phase::Retired => Vec::new(),
        }
    }

    /// The low-level counter protocol is explored independently of Rust's
    /// outer lease borrow, which further restricts reachable API executions.
    fn reader(slot: &Arc<Self>) -> Option<Reader<P>> {
        let _admission = slot.admission.lock().unwrap();
        if slot.phase(P::OBSERVE) != Phase::Live {
            return None;
        }
        slot.readers.fetch_add(1, Ordering::AcqRel);
        slot.assert_words();
        Some(Reader {
            slot: Some(Arc::clone(slot)),
            _thread: PhantomData,
        })
    }

    fn close_and_finish(&self) -> Result<(), FinishError> {
        let _admission = self.admission.try_lock().map_err(|_| FinishError::Busy)?;
        match self.phase(Ordering::Acquire) {
            Phase::Live => self.store_phase(Phase::Closing, Ordering::Release),
            Phase::Closing => {}
            Phase::Reserved | Phase::Cancelled | Phase::Retired => {
                return Err(FinishError::WrongPhase);
            }
        }

        // Drop publishes abandonment before decrementing the active count.
        // Observing the final zero with Acquire must precede reading abandoned;
        // otherwise the closer could read old zero followed by the new zero.
        let (readers, abandoned) = if P::ABANDONMENT_FIRST {
            let abandoned = self.abandoned.load(Ordering::Acquire);
            let readers = self.readers.load(Ordering::Acquire);
            (readers, abandoned)
        } else {
            let readers = self.readers.load(Ordering::Acquire);
            let abandoned = self.abandoned.load(Ordering::Acquire);
            (readers, abandoned)
        };
        if P::CHECK_ACTIVE_READERS && readers != 0 {
            return Err(FinishError::ActiveReaders);
        }
        if P::CHECK_ABANDONED_READERS && abandoned != 0 {
            return Err(FinishError::AbandonedReader);
        }
        self.store_phase(Phase::Retired, Ordering::Release);
        Ok(())
    }
}

/// One counted retained epoch. Its destructor models the logical obligation:
/// Live/Closing abandonment retains the count permanently; terminal states
/// release it. Production retains ownership (including the actual Arc), not
/// merely this model's counter. The table's observational Slot Arc is separate.
#[derive(Debug)]
struct Retention<P: Protocol> {
    slot: Arc<Slot<P>>,
}

assert_impl_all!(Retention<Production>: Send, Sync, fmt::Debug);
assert_not_impl_any!(Retention<Production>: Clone, Copy);

impl<P: Protocol> Drop for Retention<P> {
    fn drop(&mut self) {
        match self.slot.phase(Ordering::Acquire) {
            Phase::Cancelled | Phase::Retired => {
                self.slot.epoch.held.fetch_sub(1, Ordering::Release);
            }
            Phase::Reserved | Phase::Live | Phase::Closing => {}
        }
    }
}

#[derive(Debug)]
struct Prepared<P: Protocol> {
    retention: Option<Retention<P>>,
    _thread: PhantomData<Rc<()>>,
}

assert_not_impl_any!(Prepared<Production>: Send, Sync, Clone, Copy);
assert_impl_all!(Prepared<Production>: fmt::Debug);

impl<P: Protocol> Prepared<P> {
    fn reserve(exclusion: &Exclusion, generation: Generation) -> Self {
        exclusion.epoch.held.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(Slot {
            phase: AtomicU8::new(Phase::Reserved.into()),
            admission: Mutex::new(()),
            readers: AtomicUsize::new(0),
            abandoned: AtomicUsize::new(0),
            words: std::array::from_fn(|_| AtomicUsize::new(0)),
            payload_initialized: AtomicBool::new(false),
            generation,
            epoch: Arc::clone(&exclusion.epoch),
            _protocol: PhantomData,
        });
        Self {
            retention: Some(Retention { slot }),
            _thread: PhantomData,
        }
    }

    fn finalize(mut self) -> Finalized<P> {
        let retention = self.retention.take().unwrap();
        for (word, value) in retention.slot.words.iter().zip(INPUT) {
            word.store(value.root_word().unwrap_or(0), Ordering::Relaxed);
        }
        Finalized {
            retention: Some(retention),
            _thread: PhantomData,
        }
    }
}

impl<P: Protocol> Drop for Prepared<P> {
    fn drop(&mut self) {
        if let Some(retention) = &self.retention {
            retention
                .slot
                .store_phase(Phase::Cancelled, Ordering::Release);
        }
    }
}

#[derive(Debug)]
struct Finalized<P: Protocol> {
    retention: Option<Retention<P>>,
    _thread: PhantomData<Rc<()>>,
}

assert_not_impl_any!(Finalized<Production>: Send, Sync, Clone, Copy);
assert_impl_all!(Finalized<Production>: fmt::Debug);

impl<P: Protocol> Finalized<P> {
    fn publish(mut self) -> Lease<P> {
        let retention = self.retention.take().unwrap();
        match P::PUBLICATION_LAYER {
            PublicationLayer::TableAndOnce => {
                let _admission = retention.slot.admission.lock().unwrap();
                retention
                    .slot
                    .payload_initialized
                    .store(true, Ordering::Release);
                retention.slot.store_phase(Phase::Live, P::PUBLISH);
            }
            PublicationLayer::StateOnly => {
                retention.slot.store_phase(Phase::Live, P::PUBLISH);
            }
        }
        Lease { retention }
    }
}

impl<P: Protocol> Drop for Finalized<P> {
    fn drop(&mut self) {
        if let Some(retention) = &self.retention {
            retention
                .slot
                .store_phase(Phase::Cancelled, Ordering::Release);
        }
    }
}

#[derive(Debug)]
struct Lease<P: Protocol> {
    retention: Retention<P>,
}

assert_impl_all!(Lease<Production>: Send, Sync, fmt::Debug);
assert_not_impl_any!(Lease<Production>: Clone, Copy);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishError {
    Busy,
    ActiveReaders,
    AbandonedReader,
    WrongPhase,
}

#[derive(Debug)]
struct FinishFailure<P: Protocol> {
    lease: Lease<P>,
    error: FinishError,
}

assert_impl_all!(FinishFailure<Production>: Send, Sync, fmt::Debug);
assert_not_impl_any!(FinishFailure<Production>: Clone, Copy);

#[derive(Clone, Copy, Debug)]
struct Retired {
    generation: Generation,
}

assert_impl_all!(Retired: Send, Sync, Clone, Copy, fmt::Debug);

impl<P: Protocol> Lease<P> {
    fn try_finish(self) -> Result<Retired, FinishFailure<P>> {
        match self.retention.slot.close_and_finish() {
            Ok(()) => Ok(Retired {
                generation: self.retention.slot.generation,
            }),
            Err(error) => Err(FinishFailure { lease: self, error }),
        }
    }
}

#[derive(Debug)]
struct Reader<P: Protocol> {
    slot: Option<Arc<Slot<P>>>,
    _thread: PhantomData<Rc<()>>,
}

assert_not_impl_any!(Reader<Production>: Send, Sync, Clone, Copy);
assert_impl_all!(Reader<Production>: fmt::Debug);

impl<P: Protocol> Reader<P> {
    fn read(&self) {
        let slot = self.slot.as_ref().unwrap();
        assert!(
            matches!(slot.phase(Ordering::Acquire), Phase::Live | Phase::Closing),
            "retired a batch with an active reader"
        );
        slot.assert_words();
    }

    fn finish(mut self) {
        let slot = self.slot.take().unwrap();
        slot.readers.fetch_sub(1, Ordering::Release);
    }
}

impl<P: Protocol> Drop for Reader<P> {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            slot.abandoned.fetch_add(1, Ordering::Release);
            slot.readers.fetch_sub(1, Ordering::Release);
        }
    }
}

fn check(model: impl Fn() + Send + Sync + 'static) {
    check_threads(2, model);
}

fn check_threads(threads: usize, model: impl Fn() + Send + Sync + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.max_threads = threads;
    builder.max_branches = 256;
    builder.max_permutations = None;
    builder.max_duration = None;
    // Keep Builder::new()'s LOOM_MAX_PREEMPTIONS setting; do not silently
    // replace the requested bound with an unbounded exploration.
    builder.checkpoint_file = None;
    builder.expect_explicit_explore = false;
    builder.location = true;
    builder.log = false;
    struct Iterations(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Iterations {
        fn drop(&mut self) {
            eprintln!(
                "LOOM_ITERATIONS {} {}",
                std::thread::current().name().unwrap_or("unnamed"),
                self.0.load(std::sync::atomic::Ordering::Relaxed)
            );
        }
    }
    let iterations = Iterations(std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let count = std::sync::Arc::clone(&iterations.0);
    builder.check(move || {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        model();
    });
}

fn live<P: Protocol>() -> (Arc<Epoch>, Arc<Slot<P>>, Lease<P>) {
    let epoch = Epoch::new();
    let exclusion = Exclusion::acquire(&epoch);
    let prepared = Prepared::<P>::reserve(&exclusion, Generation::First);
    let slot = Arc::clone(&prepared.retention.as_ref().unwrap().slot);
    let lease = prepared.finalize().publish();
    drop(exclusion);
    (epoch, slot, lease)
}

fn publication<P: Protocol>() {
    check(|| {
        let epoch = Epoch::new();
        let exclusion = Exclusion::acquire(&epoch);
        let prepared = Prepared::<P>::reserve(&exclusion, Generation::First);
        let slot = Arc::clone(&prepared.retention.as_ref().unwrap().slot);
        // Prepared/Finalized stay on their creating thread. The shared table
        // slot reaches the scanner before publication, without a join edge.
        let scanned = Arc::clone(&slot);
        let reader = thread::spawn(move || {
            if let Some(reader) = Slot::reader(&scanned) {
                reader.read();
                reader.finish();
            }
        });
        let lease = prepared.finalize().publish();
        drop(exclusion);
        reader.join().unwrap();
        assert_eq!(slot.trace(), vec![11, 23]);
        assert!(!epoch.may_launch());
        lease.try_finish().unwrap();
        assert!(epoch.may_launch());
    });
}

#[test]
fn release_acquire_publishes_the_complete_immutable_batch() {
    publication::<Production>();
}

#[test]
fn state_only_experiment_retains_release_acquire_without_extra_publication_edges() {
    publication::<StatePublicationOnly>();
}

#[test]
fn cancelling_reserved_and_finalized_batches_releases_only_their_epoch() {
    check(|| {
        let epoch = Epoch::new();
        for finalize in [false, true] {
            let exclusion = Exclusion::acquire(&epoch);
            let prepared = Prepared::<Production>::reserve(&exclusion, Generation::First);
            let slot = Arc::clone(&prepared.retention.as_ref().unwrap().slot);
            drop(exclusion);
            // Reserved/Finalized must retain exclusion independently of the
            // admission supplied to reserve, not merely borrow its lifetime.
            assert!(!epoch.may_launch());
            if finalize {
                let finalized = prepared.finalize();
                assert!(!epoch.may_launch());
                drop(finalized);
            } else {
                drop(prepared);
            }
            assert_eq!(slot.phase(Ordering::Acquire), Phase::Cancelled);
            assert!(Slot::reader(&slot).is_none());
            assert!(slot.trace().is_empty());
            assert!(epoch.may_launch());
        }
        let exclusion = Exclusion::acquire(&epoch);
        drop(Prepared::<Production>::reserve(
            &exclusion,
            Generation::First,
        ));
        // Cancelling a batch cannot release its caller's separate admission.
        assert!(!epoch.may_launch());
        drop(exclusion);
        assert!(epoch.may_launch());
    });
}

#[test]
fn close_blocks_new_readers_but_keeps_existing_readers_and_gc_roots() {
    check(|| {
        let (epoch, slot, lease) = live::<Production>();
        let reader = Slot::reader(&slot).unwrap();
        let failure = lease.try_finish().unwrap_err();
        assert_eq!(failure.error, FinishError::ActiveReaders);
        assert_eq!(slot.phase(Ordering::Acquire), Phase::Closing);
        assert!(Slot::reader(&slot).is_none());
        assert_eq!(slot.trace(), vec![11, 23]);
        reader.read();
        assert!(!epoch.may_launch());
        reader.finish();
        let retired = failure.lease.try_finish().unwrap();
        assert_eq!(retired.generation, Generation::First);
        assert!(slot.trace().is_empty());
        // Retaining the retired node's storage does not retain the exclusion.
        assert!(epoch.may_launch());
    });
}

#[test]
fn reader_drop_is_abandonment_and_cannot_complete_retirement() {
    check(|| {
        let (epoch, slot, lease) = live::<Production>();
        drop(Slot::reader(&slot).unwrap());
        let failure = lease.try_finish().unwrap_err();
        assert_eq!(failure.error, FinishError::AbandonedReader);
        assert_eq!(slot.phase(Ordering::Acquire), Phase::Closing);
        assert_eq!(slot.trace(), vec![11, 23]);
        drop(failure.lease);
        assert_eq!(slot.trace(), vec![11, 23]);
        assert!(!epoch.may_launch());
    });
}

#[test]
fn unfinished_lease_drop_keeps_live_roots_and_exclusion() {
    check(|| {
        let (epoch, slot, lease) = live::<Production>();
        drop(lease);
        assert_eq!(slot.phase(Ordering::Acquire), Phase::Live);
        assert_eq!(slot.trace(), vec![11, 23]);
        assert!(!epoch.may_launch());
    });
}

fn abandonment_race<P: Protocol>() {
    check(|| {
        let (epoch, slot, lease) = live::<P>();
        let worker_slot = Arc::clone(&slot);
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let reader = Slot::reader(&worker_slot).unwrap();
            ready_tx.send(()).unwrap();
            drop(reader);
        });
        ready_rx.recv().unwrap();
        let result = lease.try_finish();
        worker.join().unwrap();
        assert!(
            slot.phase(Ordering::Acquire) != Phase::Retired,
            "retired a batch without explicit reader finish"
        );
        let failure = result.unwrap_err();
        assert!(matches!(
            failure.error,
            FinishError::ActiveReaders | FinishError::AbandonedReader
        ));
        assert_eq!(slot.trace(), vec![11, 23]);
        drop(failure.lease);
        assert!(!epoch.may_launch());
    });
}

#[test]
fn acquire_reader_zero_orders_the_prior_abandonment_store() {
    abandonment_race::<Production>();
}

#[test]
fn explicit_reader_finish_racing_close_preserves_failed_finish_ownership() {
    check(|| {
        let (epoch, slot, lease) = live::<Production>();
        let worker_slot = Arc::clone(&slot);
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let reader = Slot::reader(&worker_slot).unwrap();
            reader.read();
            ready_tx.send(()).unwrap();
            reader.finish();
        });
        ready_rx.recv().unwrap();
        let result = lease.try_finish();
        worker.join().unwrap();
        let retired = match result {
            Ok(retired) => retired,
            Err(failure) => {
                assert_eq!(failure.error, FinishError::ActiveReaders);
                assert_eq!(slot.phase(Ordering::Acquire), Phase::Closing);
                assert_eq!(slot.trace(), vec![11, 23]);
                failure.lease.try_finish().unwrap()
            }
        };
        assert_eq!(retired.generation, Generation::First);
        assert!(slot.trace().is_empty());
        assert!(epoch.may_launch());
    });
}

#[test]
fn busy_retirement_preserves_the_live_lease_and_exclusion_for_retry() {
    check(|| {
        let (epoch, slot, lease) = live::<Production>();
        let worker_slot = Arc::clone(&slot);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let admission = worker_slot.admission.lock().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(admission);
        });
        ready_rx.recv().unwrap();
        let failure = lease.try_finish().unwrap_err();
        assert_eq!(failure.error, FinishError::Busy);
        assert_eq!(slot.phase(Ordering::Acquire), Phase::Live);
        assert!(!epoch.may_launch());
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(slot.trace(), vec![11, 23]);
        failure.lease.try_finish().unwrap();
        assert!(slot.trace().is_empty());
        assert!(epoch.may_launch());
    });
}

#[test]
fn recycling_uses_a_new_control_and_generation() {
    check(|| {
        let epoch = Epoch::new();
        let exclusion = Exclusion::acquire(&epoch);
        let first = Prepared::<Production>::reserve(&exclusion, Generation::First);
        let old_control = Arc::clone(&first.retention.as_ref().unwrap().slot);
        drop(first);
        let second = Prepared::<Production>::reserve(&exclusion, Generation::Second);
        let new_control = Arc::clone(&second.retention.as_ref().unwrap().slot);
        assert!(!Arc::ptr_eq(&old_control, &new_control));
        let lease = second.finalize().publish();
        drop(exclusion);
        assert_eq!(old_control.phase(Ordering::Acquire), Phase::Cancelled);
        assert_eq!(new_control.phase(Ordering::Acquire), Phase::Live);
        assert_eq!(new_control.trace(), vec![11, 23]);
        assert!(!epoch.may_launch());
        let retired = lease.try_finish().unwrap();
        assert_eq!(retired.generation, Generation::Second);
        assert!(epoch.may_launch());
    });
}

#[test]
#[should_panic(expected = "accepted unpublished batch payload")]
fn negative_control_relaxed_publication_exposes_unpublished_words() {
    publication::<RelaxedPublication>();
}

#[test]
#[should_panic(expected = "accepted unpublished batch payload")]
fn negative_control_relaxed_observation_exposes_unpublished_words() {
    publication::<RelaxedObservation>();
}

#[test]
#[should_panic(expected = "retired a batch with an active reader")]
fn negative_control_premature_retirement_removes_an_active_root() {
    check(|| {
        let (_, slot, lease) = live::<PrematureRetirement>();
        let reader = Slot::reader(&slot).unwrap();
        let _retired = lease.try_finish().unwrap();
        reader.read();
        reader.finish();
    });
}

#[test]
#[should_panic(expected = "retired a batch without explicit reader finish")]
fn negative_control_drop_must_not_count_as_explicit_finish() {
    abandonment_race::<DropCountsAsFinish>();
}

#[test]
#[should_panic(expected = "retired a batch without explicit reader finish")]
fn negative_control_abandonment_first_can_accept_two_unrelated_zeroes() {
    abandonment_race::<WrongCounterOrder>();
}

/// Part B's in-place reuse model. Unlike the Part A state-word experiments,
/// this models the actual table/control/payload Arc ownership checks. Plain
/// payload memory is immutable outside a unique Prepared endpoint; the table
/// mutex serializes scans, admissions, retirement and generation replacement.
mod reuse {
    use super::*;

    #[derive(Debug)]
    struct Control {
        generation: usize,
        phase: AtomicU8,
        readers: AtomicUsize,
        abandoned: AtomicUsize,
    }

    impl Control {
        fn phase(&self) -> Phase {
            Phase::try_from(self.phase.load(Ordering::Acquire)).unwrap()
        }

        fn set_phase(&self, phase: Phase) {
            self.phase.store(phase.into(), Ordering::Release);
        }
    }

    #[derive(Debug)]
    struct Payload {
        words: Vec<usize>,
        used: usize,
    }

    #[derive(Debug)]
    struct Table {
        control: Arc<Control>,
        payload: Option<Arc<Payload>>,
        next_generation: usize,
    }

    impl Table {
        fn trace(table: &Arc<Mutex<Self>>) -> (usize, Vec<usize>) {
            let table = table.lock().unwrap();
            let roots = match table.control.phase() {
                Phase::Live | Phase::Closing => {
                    let payload = table.payload.as_ref().unwrap();
                    payload.words[..payload.used].to_vec()
                }
                Phase::Reserved | Phase::Retired | Phase::Cancelled => Vec::new(),
            };
            (table.control.generation, roots)
        }

        fn reader(table: &Arc<Mutex<Self>>, generation: usize) -> Option<ScopedReader> {
            let table = table.lock().unwrap();
            // This is the table key validation, before any counter increment
            // or payload clone. A copied generation is metadata, not a reader.
            if table.control.generation != generation || table.control.phase() != Phase::Live {
                return None;
            }
            table.control.readers.fetch_add(1, Ordering::AcqRel);
            Some(ScopedReader {
                control: Arc::clone(&table.control),
                payload: Arc::clone(table.payload.as_ref().unwrap()),
                active: true,
                _thread: PhantomData,
            })
        }
    }

    #[derive(Debug)]
    struct Ownership {
        table: Arc<Mutex<Table>>,
        control: Option<Arc<Control>>,
        epoch: Arc<Epoch>,
    }

    impl Drop for Ownership {
        fn drop(&mut self) {
            match self.control.as_ref().unwrap().phase() {
                Phase::Cancelled | Phase::Retired => {
                    self.epoch.held.fetch_sub(1, Ordering::Release);
                }
                // Model the logical obligation retained by unfinished Drop.
                Phase::Reserved | Phase::Live | Phase::Closing => {}
            }
        }
    }

    #[derive(Debug)]
    struct ReusablePrepared {
        ownership: Option<Ownership>,
        payload: Arc<Payload>,
        _thread: PhantomData<Rc<()>>,
    }

    assert_not_impl_any!(ReusablePrepared: Send, Sync, Clone, Copy);

    impl ReusablePrepared {
        fn ensure_capacity(
            &mut self,
            capacity: usize,
            allocation_succeeds: bool,
        ) -> Result<(), ()> {
            let payload = Arc::get_mut(&mut self.payload).ok_or(())?;
            assert_eq!(payload.used, 0);
            if capacity > payload.words.len() {
                // Inject host allocation failure before replacing any storage.
                if !allocation_succeeds {
                    return Err(());
                }
                payload.words = vec![0; capacity];
            }
            Ok(())
        }

        fn publish(mut self, roots: &[usize]) -> ReusableLease {
            let payload = Arc::get_mut(&mut self.payload).unwrap();
            assert!(roots.len() <= payload.words.len());
            payload.words[..roots.len()].copy_from_slice(roots);
            payload.used = roots.len();
            let ownership = self.ownership.take().unwrap();
            {
                let mut table = ownership.table.lock().unwrap();
                assert!(Arc::ptr_eq(
                    &table.control,
                    ownership.control.as_ref().unwrap()
                ));
                assert_eq!(table.control.phase(), Phase::Reserved);
                assert!(table.payload.is_none());
                table.payload = Some(Arc::clone(&self.payload));
                table.control.set_phase(Phase::Live);
            }
            ReusableLease {
                ownership: Some(ownership),
            }
        }
    }

    impl Drop for ReusablePrepared {
        fn drop(&mut self) {
            if let Some(ownership) = &self.ownership {
                ownership
                    .control
                    .as_ref()
                    .unwrap()
                    .set_phase(Phase::Cancelled);
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RecycleError {
        Busy,
        Readers,
        NonUnique,
        GenerationExhausted,
    }

    #[derive(Debug)]
    struct RecycleFailure {
        lease: ReusableLease,
        error: RecycleError,
    }

    #[derive(Debug)]
    struct ReusableLease {
        ownership: Option<Ownership>,
    }

    assert_impl_all!(ReusableLease: Send, Sync);
    assert_not_impl_any!(ReusableLease: Clone, Copy);

    impl ReusableLease {
        fn try_recycle(mut self) -> Result<ReusablePrepared, RecycleFailure> {
            let result = (|| {
                let ownership = self.ownership.as_mut().unwrap();
                let table_arc = Arc::clone(&ownership.table);
                let mut table = table_arc.try_lock().map_err(|_| RecycleError::Busy)?;
                let control = ownership.control.as_ref().unwrap();
                assert!(Arc::ptr_eq(&table.control, control));
                assert_eq!(table.control.generation, control.generation);
                assert!(matches!(control.phase(), Phase::Live | Phase::Closing));
                control.set_phase(Phase::Closing);
                let readers = control.readers.load(Ordering::Acquire);
                let abandoned = control.abandoned.load(Ordering::Acquire);
                if readers != 0 || abandoned != 0 {
                    return Err(RecycleError::Readers);
                }
                // Loom Arc has no weak references. Its strong-count check
                // models both ownership counts; weak observers remain covered
                // by production's explicit weak_count checks and tests.
                if Arc::strong_count(control) != 2
                    || Arc::strong_count(table.payload.as_ref().unwrap()) != 1
                {
                    return Err(RecycleError::NonUnique);
                }
                let generation = table
                    .next_generation
                    .checked_add(1)
                    .ok_or(RecycleError::GenerationExhausted)?;
                control.set_phase(Phase::Retired);
                drop(ownership.control.take());
                // No table observer can appear while this guard is held.
                let mut payload = table.payload.take().unwrap();
                let control = Arc::get_mut(&mut table.control).unwrap();
                let words = Arc::get_mut(&mut payload).unwrap();
                words.words[..words.used].fill(0);
                words.used = 0;
                control.generation = generation;
                control.set_phase(Phase::Reserved);
                table.next_generation = generation;
                ownership.control = Some(Arc::clone(&table.control));
                Ok(payload)
            })();
            match result {
                Ok(payload) => Ok(ReusablePrepared {
                    ownership: self.ownership.take(),
                    payload,
                    _thread: PhantomData,
                }),
                Err(error) => Err(RecycleFailure { lease: self, error }),
            }
        }

        fn finish(mut self) {
            let ownership = self.ownership.as_ref().unwrap();
            let table = ownership.table.lock().unwrap();
            assert!(Arc::ptr_eq(
                &table.control,
                ownership.control.as_ref().unwrap()
            ));
            assert_eq!(table.control.readers.load(Ordering::Acquire), 0);
            assert_eq!(table.control.abandoned.load(Ordering::Acquire), 0);
            table.control.set_phase(Phase::Retired);
            drop(table);
            drop(self.ownership.take());
        }
    }

    #[derive(Debug)]
    struct ScopedReader {
        control: Arc<Control>,
        payload: Arc<Payload>,
        active: bool,
        _thread: PhantomData<Rc<()>>,
    }

    assert_not_impl_any!(ScopedReader: Send, Sync, Clone, Copy);

    impl ScopedReader {
        fn read(&self) {
            assert!(matches!(self.control.phase(), Phase::Live | Phase::Closing));
            assert_eq!(self.payload.words[..self.payload.used], [11, 23]);
        }

        fn finish(mut self) {
            self.control.readers.fetch_sub(1, Ordering::Release);
            self.active = false;
        }
    }

    impl Drop for ScopedReader {
        fn drop(&mut self) {
            if self.active {
                self.control.abandoned.fetch_add(1, Ordering::Release);
                self.control.readers.fetch_sub(1, Ordering::Release);
            }
        }
    }

    fn initial() -> (Arc<Epoch>, Arc<Mutex<Table>>, ReusableLease) {
        let epoch = Epoch::new();
        let exclusion = Exclusion::acquire(&epoch);
        epoch.held.fetch_add(1, Ordering::Relaxed);
        let control = Arc::new(Control {
            generation: 1,
            phase: AtomicU8::new(Phase::Reserved.into()),
            readers: AtomicUsize::new(0),
            abandoned: AtomicUsize::new(0),
        });
        let table = Arc::new(Mutex::new(Table {
            control: Arc::clone(&control),
            payload: None,
            next_generation: 1,
        }));
        let prepared = ReusablePrepared {
            ownership: Some(Ownership {
                table: Arc::clone(&table),
                control: Some(control),
                epoch: Arc::clone(&epoch),
            }),
            payload: Arc::new(Payload {
                words: vec![0; 2],
                used: 0,
            }),
            _thread: PhantomData,
        };
        let lease = prepared.publish(&[11, 23]);
        drop(exclusion);
        (epoch, table, lease)
    }

    #[test]
    fn recycle_races_reader_admission_finish_and_table_scan() {
        check_threads(3, || {
            let (epoch, table, lease) = initial();
            let read_table = Arc::clone(&table);
            let reader = thread::spawn(move || {
                if let Some(reader) = Table::reader(&read_table, 1) {
                    reader.read();
                    reader.finish();
                }
            });
            let scan_table = Arc::clone(&table);
            let scanner = thread::spawn(move || {
                let (generation, roots) = Table::trace(&scan_table);
                match generation {
                    1 => assert_eq!(roots, [11, 23]),
                    2 => assert!(roots.is_empty()),
                    _ => panic!("unexpected generation"),
                }
            });
            let recycled = lease.try_recycle();
            reader.join().unwrap();
            scanner.join().unwrap();
            let prepared = match recycled {
                Ok(prepared) => prepared,
                Err(failure) => {
                    assert!(matches!(
                        failure.error,
                        RecycleError::Busy | RecycleError::Readers | RecycleError::NonUnique
                    ));
                    assert_eq!(Table::trace(&table), (1, vec![11, 23]));
                    assert!(!epoch.may_launch());
                    failure.lease.try_recycle().unwrap()
                }
            };
            assert_eq!(Table::trace(&table), (2, vec![]));
            assert!(Table::reader(&table, 1).is_none());
            assert!(!epoch.may_launch());
            let lease = prepared.publish(&[11, 23]);
            assert!(Table::reader(&table, 1).is_none());
            let reader = Table::reader(&table, 2).unwrap();
            reader.read();
            reader.finish();
            lease.finish();
            assert!(epoch.may_launch());
        });
    }

    #[test]
    fn recycle_generation_growth_and_allocation_failure_preserve_storage() {
        check(|| {
            let (epoch, table, lease) = initial();
            let old_generation = table.lock().unwrap().control.generation;
            let mut prepared = lease.try_recycle().unwrap();
            assert_eq!(prepared.payload.words, [0, 0]);
            assert_eq!(prepared.ensure_capacity(5, false), Err(()));
            assert_eq!(prepared.payload.words, [0, 0]);
            assert!(!epoch.may_launch());
            prepared.ensure_capacity(5, true).unwrap();
            assert_eq!(prepared.payload.words, [0; 5]);
            prepared.ensure_capacity(1, false).unwrap();
            assert_eq!(prepared.payload.words.len(), 5);
            let lease = prepared.publish(&[11, 23, 37, 41, 43]);
            assert!(Table::reader(&table, old_generation).is_none());
            assert_eq!(Table::trace(&table), (2, vec![11, 23, 37, 41, 43]));
            lease.finish();
            assert!(epoch.may_launch());
        });
    }

    #[test]
    fn recycle_active_reader_and_nonunique_observers_retain_the_lease_for_retry() {
        check(|| {
            let (epoch, table, lease) = initial();
            let reader = Table::reader(&table, 1).unwrap();
            let failure = lease.try_recycle().unwrap_err();
            assert_eq!(failure.error, RecycleError::Readers);
            assert_eq!(Table::trace(&table), (1, vec![11, 23]));
            assert!(Table::reader(&table, 1).is_none());
            reader.read();
            reader.finish();
            let control_observer = Arc::clone(&table.lock().unwrap().control);
            let failure = failure.lease.try_recycle().unwrap_err();
            assert_eq!(failure.error, RecycleError::NonUnique);
            drop(control_observer);
            let payload_observer = Arc::clone(table.lock().unwrap().payload.as_ref().unwrap());
            let failure = failure.lease.try_recycle().unwrap_err();
            assert_eq!(failure.error, RecycleError::NonUnique);
            assert_eq!(Table::trace(&table), (1, vec![11, 23]));
            assert!(!epoch.may_launch());
            drop(payload_observer);
            let prepared = failure.lease.try_recycle().unwrap();
            assert_eq!(Table::trace(&table), (2, vec![]));
            drop(prepared);
            assert!(epoch.may_launch());
        });
    }

    #[test]
    fn recycle_abandoned_reader_retains_roots_and_exclusion() {
        check(|| {
            let (epoch, table, lease) = initial();
            drop(Table::reader(&table, 1).unwrap());
            let failure = lease.try_recycle().unwrap_err();
            assert_eq!(failure.error, RecycleError::Readers);
            assert_eq!(Table::trace(&table), (1, vec![11, 23]));
            drop(failure.lease);
            assert_eq!(Table::trace(&table), (1, vec![11, 23]));
            assert!(!epoch.may_launch());
        });
    }

    #[test]
    fn recycle_busy_table_retains_live_lease_for_retry() {
        check(|| {
            let (epoch, table, lease) = initial();
            let scanned = Arc::clone(&table);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let scanner = thread::spawn(move || {
                let table = scanned.lock().unwrap();
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                assert_eq!(table.control.phase(), Phase::Live);
                assert_eq!(table.payload.as_ref().unwrap().used, 2);
            });
            ready_rx.recv().unwrap();
            let failure = lease.try_recycle().unwrap_err();
            assert_eq!(failure.error, RecycleError::Busy);
            assert!(!epoch.may_launch());
            release_tx.send(()).unwrap();
            scanner.join().unwrap();
            assert_eq!(Table::trace(&table), (1, vec![11, 23]));
            drop(failure.lease.try_recycle().unwrap());
            assert!(epoch.may_launch());
        });
    }

    #[test]
    fn recycle_exhausted_generation_retains_roots_until_explicit_finish() {
        check(|| {
            let (epoch, table, lease) = initial();
            table.lock().unwrap().next_generation = usize::MAX;
            let failure = lease.try_recycle().unwrap_err();
            assert_eq!(failure.error, RecycleError::GenerationExhausted);
            assert_eq!(Table::trace(&table), (1, vec![11, 23]));
            assert!(!epoch.may_launch());
            failure.lease.finish();
            assert_eq!(Table::trace(&table), (1, vec![]));
            assert!(epoch.may_launch());
        });
    }

    /// Collector state and the restored census are serialized by the owner
    /// seat. The guard below models that sole-writer proof, not a production
    /// mutex or a concurrent sweeper. Cooperative sweep uses already marked
    /// objects and never consults the root table. Starting another mark must
    /// acquire the seat and census the restored owner again.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum CollectionPhase {
        Idle,
        Mark,
        Sweep,
    }

    #[derive(Debug)]
    struct CollectorState {
        phase: CollectionPhase,
        synchronous_extent: bool,
        concurrent_marker: bool,
        restored_roots: Option<Vec<usize>>,
    }

    impl CollectorState {
        fn sweeping() -> Self {
            Self {
                phase: CollectionPhase::Sweep,
                synchronous_extent: false,
                concurrent_marker: false,
                restored_roots: None,
            }
        }

        fn roots(&self, table: &Arc<Mutex<Table>>) -> Vec<usize> {
            let (_, mut roots) = Table::trace(table);
            if let Some(restored) = &self.restored_roots {
                roots.extend(restored);
            }
            roots.sort_unstable();
            roots.dedup();
            roots
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RetirementProofError {
        UnrestoredOwner,
        Marking,
        SynchronousExtent,
        ConcurrentMarker,
    }

    #[derive(Debug)]
    struct SweepRetirement<'a> {
        _seat: loom::sync::MutexGuard<'a, CollectorState>,
    }

    impl<'a> SweepRetirement<'a> {
        fn admit(collector: &'a Mutex<CollectorState>) -> Result<Self, RetirementProofError> {
            let seat = collector.lock().unwrap();
            if seat.restored_roots.is_none() {
                return Err(RetirementProofError::UnrestoredOwner);
            }
            if seat.phase == CollectionPhase::Mark {
                return Err(RetirementProofError::Marking);
            }
            if seat.synchronous_extent {
                return Err(RetirementProofError::SynchronousExtent);
            }
            if seat.concurrent_marker {
                return Err(RetirementProofError::ConcurrentMarker);
            }
            Ok(Self { _seat: seat })
        }

        fn recycle(&self, lease: ReusableLease) -> Result<ReusablePrepared, RecycleFailure> {
            lease.try_recycle()
        }

        fn finish(&self, lease: ReusableLease) {
            lease.finish();
        }
    }

    fn pending_sweep_retirement(recycle: bool) {
        check(move || {
            let (epoch, table, lease) = initial();
            let collector = Arc::new(Mutex::new(CollectorState::sweeping()));
            let next_collector = Arc::clone(&collector);
            let next_table = Arc::clone(&table);
            let mut next_cycle = Some(thread::spawn(move || {
                // Finishing a sweep has no root-table access. The next
                // mark can begin before or after restoration/retirement.
                next_collector.lock().unwrap().phase = CollectionPhase::Idle;
                {
                    let mut seat = next_collector.lock().unwrap();
                    seat.phase = CollectionPhase::Mark;
                    assert_eq!(seat.roots(&next_table), [11, 23]);
                }
                thread::yield_now();
                next_collector.lock().unwrap().phase = CollectionPhase::Idle;
            }));

            collector.lock().unwrap().restored_roots = Some(vec![11, 23]);
            let outcome = match SweepRetirement::admit(&collector) {
                Ok(proof) => {
                    if recycle {
                        Some(proof.recycle(lease).unwrap())
                    } else {
                        proof.finish(lease);
                        None
                    }
                }
                Err(error) => {
                    assert_eq!(error, RetirementProofError::Marking);
                    // Failed proof admission leaves the exact live lease
                    // and exclusion intact until the marker completes.
                    assert_eq!(Table::trace(&table).1, [11, 23]);
                    assert!(!epoch.may_launch());
                    next_cycle.take().unwrap().join().unwrap();
                    let proof = SweepRetirement::admit(&collector).unwrap();
                    if recycle {
                        Some(proof.recycle(lease).unwrap())
                    } else {
                        proof.finish(lease);
                        None
                    }
                }
            };
            if let Some(next_cycle) = next_cycle {
                next_cycle.join().unwrap();
            }
            drop(outcome);
            assert!(Table::trace(&table).1.is_empty());
            assert_eq!(collector.lock().unwrap().roots(&table), [11, 23]);
            assert!(epoch.may_launch());
        });
    }

    #[test]
    fn pending_sweep_finish_preserves_the_next_mark_census() {
        pending_sweep_retirement(false);
    }

    #[test]
    fn pending_sweep_recycle_preserves_the_next_mark_census() {
        pending_sweep_retirement(true);
    }

    #[test]
    fn sweep_retirement_rejects_unrestored_marking_and_reader_obligations() {
        check(|| {
            let (epoch, table, lease) = initial();
            let collector = Mutex::new(CollectorState::sweeping());
            assert_eq!(
                SweepRetirement::admit(&collector).unwrap_err(),
                RetirementProofError::UnrestoredOwner
            );
            collector.lock().unwrap().restored_roots = Some(vec![11, 23]);
            for error in [
                RetirementProofError::Marking,
                RetirementProofError::SynchronousExtent,
                RetirementProofError::ConcurrentMarker,
            ] {
                {
                    let mut seat = collector.lock().unwrap();
                    seat.phase = if error == RetirementProofError::Marking {
                        CollectionPhase::Mark
                    } else {
                        CollectionPhase::Sweep
                    };
                    seat.synchronous_extent = error == RetirementProofError::SynchronousExtent;
                    seat.concurrent_marker = error == RetirementProofError::ConcurrentMarker;
                }
                assert_eq!(SweepRetirement::admit(&collector).unwrap_err(), error);
                assert_eq!(Table::trace(&table).1, [11, 23]);
                assert!(!epoch.may_launch());
            }
            {
                let mut seat = collector.lock().unwrap();
                seat.phase = CollectionPhase::Sweep;
                seat.concurrent_marker = false;
            }
            let reader = Table::reader(&table, 1).unwrap();
            let proof = SweepRetirement::admit(&collector).unwrap();
            let failure = proof.recycle(lease).unwrap_err();
            assert_eq!(failure.error, RecycleError::Readers);
            assert_eq!(Table::trace(&table).1, [11, 23]);
            reader.read();
            reader.finish();
            drop(proof.recycle(failure.lease).unwrap());
            assert!(Table::trace(&table).1.is_empty());
            assert!(epoch.may_launch());
        });
    }

    #[test]
    #[should_panic(expected = "subsequent mark lost restored owner roots")]
    fn negative_control_sweep_alone_does_not_prove_owner_restoration() {
        check(|| {
            let (_, table, lease) = initial();
            let collector = CollectorState::sweeping();
            // Intentionally bypass proof admission. Sweep itself succeeds,
            // but clearing the batch before restoration loses the next census.
            drop(lease.try_recycle().unwrap());
            assert_eq!(
                collector.roots(&table),
                [11, 23],
                "subsequent mark lost restored owner roots"
            );
        });
    }
}
