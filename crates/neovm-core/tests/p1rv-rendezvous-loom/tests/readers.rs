#![forbid(unsafe_code)]

mod support;

use std::marker::PhantomData;
use std::rc::Rc;

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

const RESUMED_WORD: usize = 0x9000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Key {
    heap: usize,
    owner: usize,
    generation: usize,
}

struct Record {
    key: Key,
    // A distinct checksum per generation stands for an immutable root word.
    root_checksum: usize,
    readers: AtomicUsize,
    abandoned: AtomicUsize,
}

impl Record {
    fn new(generation: usize) -> Self {
        Self {
            key: Key {
                heap: 7,
                owner: 11,
                generation,
            },
            root_checksum: 0x100 + generation,
            readers: AtomicUsize::new(0),
            abandoned: AtomicUsize::new(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Live,
    Closing,
    Retired,
}

struct Slot {
    record: Arc<Record>,
    state: State,
}

struct Registry {
    slot: Mutex<Slot>,
    heap_live: AtomicBool,
    exclusion: AtomicBool,
    // Independent of the publication metadata: resuming the mutator changes
    // the object a collector is reading. Counting ACKs alone cannot save it.
    heap_word: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Attempt {
    Busy,
    Outstanding,
    Retired,
    Stale,
}

struct Reader {
    registry: Arc<Registry>,
    record: Arc<Record>,
    finished: bool,
    _confined: PhantomData<Rc<()>>,
}

static_assertions::assert_impl_all!(Registry: Send, Sync);
static_assertions::assert_impl_all!(Record: Send, Sync);
static_assertions::assert_not_impl_any!(Reader: Send, Sync, Clone, Copy);

impl Registry {
    fn new() -> Arc<Self> {
        let record = Arc::new(Record::new(1));
        Arc::new(Self {
            heap_word: AtomicUsize::new(record.root_checksum),
            slot: Mutex::new(Slot {
                record,
                state: State::Live,
            }),
            heap_live: AtomicBool::new(true),
            exclusion: AtomicBool::new(true),
        })
    }

    fn record(&self) -> Arc<Record> {
        Arc::clone(&self.slot.lock().unwrap().record)
    }

    fn admit(registry: &Arc<Self>, expected: &Arc<Record>, locked_scan: bool) -> Option<Reader> {
        let slot = registry.slot.lock().unwrap();
        if slot.record.key != expected.key || !Arc::ptr_eq(&slot.record, expected) {
            return None;
        }
        if slot.state != State::Live && !(locked_scan && slot.state == State::Closing) {
            return None;
        }
        assert!(registry.exclusion.load(Ordering::Acquire));
        assert!(registry.heap_live.load(Ordering::Acquire));
        expected.readers.fetch_add(1, Ordering::AcqRel);
        Some(Reader {
            registry: Arc::clone(registry),
            record: Arc::clone(expected),
            finished: false,
            _confined: PhantomData,
        })
    }

    // Models nonblocking transport retirement followed by the mutator's first
    // heap write. An AllStopped guard may not release while a reader remains.
    fn try_resume(&self, expected: &Arc<Record>) -> Attempt {
        let Ok(mut slot) = self.slot.try_lock() else {
            return Attempt::Busy;
        };
        if slot.record.key != expected.key || !Arc::ptr_eq(&slot.record, expected) {
            return Attempt::Stale;
        }
        if slot.state == State::Retired {
            return Attempt::Retired;
        }
        slot.state = State::Closing;
        // Observing the final Release decrement also observes an unfinished
        // Drop's earlier abandonment increment. Keep this read order.
        let readers = expected.readers.load(Ordering::Acquire);
        let abandoned = expected.abandoned.load(Ordering::Acquire);
        if readers != 0 || abandoned != 0 {
            return Attempt::Outstanding;
        }
        slot.state = State::Retired;
        self.exclusion.store(false, Ordering::Release);
        self.heap_word.store(RESUMED_WORD, Ordering::Release);
        Attempt::Retired
    }

    fn visible_roots(&self) -> usize {
        let slot = self.slot.lock().unwrap();
        match slot.state {
            State::Live | State::Closing => slot.record.root_checksum,
            State::Retired => 0,
        }
    }

    fn assert_retained(&self, expected: &Record) {
        assert!(
            self.visible_roots() == expected.root_checksum,
            "unfinished reader lost rooted publication"
        );
        assert!(
            self.heap_live.load(Ordering::Acquire),
            "unfinished reader lost heap backing"
        );
        assert!(
            self.exclusion.load(Ordering::Acquire),
            "unfinished reader lost mark exclusion"
        );
    }

    fn renew(&self) -> Arc<Record> {
        let mut slot = self.slot.lock().unwrap();
        assert_eq!(slot.state, State::Retired);
        assert_eq!(slot.record.readers.load(Ordering::Acquire), 0);
        assert_eq!(slot.record.abandoned.load(Ordering::Acquire), 0);
        let generation = slot.record.key.generation.checked_add(1).unwrap();
        let next = Arc::new(Record::new(generation));
        self.heap_word.store(next.root_checksum, Ordering::Release);
        self.exclusion.store(true, Ordering::Release);
        slot.record = Arc::clone(&next);
        slot.state = State::Live;
        next
    }

    fn finish_heap(&self) {
        let slot = self.slot.lock().unwrap();
        assert_eq!(slot.state, State::Retired);
        assert_eq!(slot.record.readers.load(Ordering::Acquire), 0);
        assert_eq!(slot.record.abandoned.load(Ordering::Acquire), 0);
        assert!(!self.exclusion.load(Ordering::Acquire));
        self.heap_live.store(false, Ordering::Release);
    }

    // Deliberately wrong variants are reachable only in negative controls.
    fn resume_ignoring_obligations(&self) {
        let mut slot = self.slot.lock().unwrap();
        slot.state = State::Retired;
        self.exclusion.store(false, Ordering::Release);
        self.heap_word.store(RESUMED_WORD, Ordering::Release);
    }

    fn admit_ignoring_generation(registry: &Arc<Self>, stale: &Arc<Record>) -> Reader {
        let slot = registry.slot.lock().unwrap();
        assert_eq!(slot.state, State::Live);
        slot.record.readers.fetch_add(1, Ordering::AcqRel);
        // The wrong decoder uses a stale caller's word with a new slot's
        // obligation. The independent root checksum must detect this mismatch.
        assert!(
            stale.root_checksum == slot.record.root_checksum,
            "stale reader decoded another generation"
        );
        Reader {
            registry: Arc::clone(registry),
            record: Arc::clone(&slot.record),
            finished: false,
            _confined: PhantomData,
        }
    }
}

impl Reader {
    fn read(&self) -> usize {
        assert!(!self.finished);
        let checksum = self.record.root_checksum;
        assert!(
            self.registry.heap_word.load(Ordering::Acquire) == checksum,
            "collector read raced with resumed mutator"
        );
        assert!(self.registry.heap_live.load(Ordering::Acquire));
        assert!(self.registry.exclusion.load(Ordering::Acquire));
        checksum
    }

    fn finish(mut self) {
        let previous = self.record.readers.fetch_sub(1, Ordering::Release);
        assert!(previous > 0, "reader completion underflow");
        self.finished = true;
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if !self.finished {
            self.record.abandoned.fetch_add(1, Ordering::Release);
            let previous = self.record.readers.fetch_sub(1, Ordering::Release);
            assert!(previous > 0, "reader abandonment underflow");
        }
    }
}

#[test]
fn final_reader_finish_precedes_resume() {
    support::check(2, || {
        let registry = Registry::new();
        let record = registry.record();
        let reader = Registry::admit(&registry, &record, false).unwrap();
        let peer_registry = Arc::clone(&registry);
        let peer_record = Arc::clone(&record);
        let peer = thread::spawn(move || peer_registry.try_resume(&peer_record));
        assert_eq!(reader.read(), record.root_checksum);
        reader.finish();
        let attempt = peer.join().unwrap();
        assert!(matches!(attempt, Attempt::Outstanding | Attempt::Retired));
        assert_eq!(registry.try_resume(&record), Attempt::Retired);
        assert_eq!(registry.visible_roots(), 0);
        registry.finish_heap();
    });
}

#[test]
fn finishing_one_of_two_readers_does_not_release_last_reader() {
    support::check(2, || {
        let registry = Registry::new();
        let record = registry.record();
        let first = Registry::admit(&registry, &record, false).unwrap();
        let last = Registry::admit(&registry, &record, false).unwrap();
        assert_eq!(first.read(), record.root_checksum);
        first.finish();
        let peer_registry = Arc::clone(&registry);
        let peer_record = Arc::clone(&record);
        let peer = thread::spawn(move || peer_registry.try_resume(&peer_record));
        assert_eq!(peer.join().unwrap(), Attempt::Outstanding);
        assert_eq!(record.readers.load(Ordering::Acquire), 1);
        registry.assert_retained(&record);
        assert_eq!(last.read(), record.root_checksum);
        last.finish();
        assert_eq!(registry.try_resume(&record), Attempt::Retired);
        registry.finish_heap();
    });
}

#[test]
fn admission_and_retirement_share_one_gate() {
    support::check(2, || {
        let registry = Registry::new();
        let record = registry.record();
        let peer_registry = Arc::clone(&registry);
        let peer_record = Arc::clone(&record);
        let peer = thread::spawn(move || {
            if let Some(reader) = Registry::admit(&peer_registry, &peer_record, false) {
                assert_eq!(reader.read(), peer_record.root_checksum);
                reader.finish();
            }
        });
        let attempt = registry.try_resume(&record);
        assert!(matches!(
            attempt,
            Attempt::Busy | Attempt::Outstanding | Attempt::Retired
        ));
        peer.join().unwrap();
        assert_eq!(registry.try_resume(&record), Attempt::Retired);
        assert!(Registry::admit(&registry, &record, false).is_none());
        registry.finish_heap();
    });
}

#[test]
fn closing_roots_allow_finished_collector_scan_but_refuse_new_escaping_reader() {
    support::check(1, || {
        let registry = Registry::new();
        let record = registry.record();
        let reader = Registry::admit(&registry, &record, false).unwrap();
        assert_eq!(registry.try_resume(&record), Attempt::Outstanding);
        registry.assert_retained(&record);
        assert!(Registry::admit(&registry, &record, false).is_none());
        let scanner = Registry::admit(&registry, &record, true).unwrap();
        assert_eq!(scanner.read(), record.root_checksum);
        scanner.finish();
        assert_eq!(registry.try_resume(&record), Attempt::Outstanding);
        assert_eq!(reader.read(), record.root_checksum);
        reader.finish();
        assert_eq!(registry.try_resume(&record), Attempt::Retired);
        registry.finish_heap();
    });
}

#[test]
fn abandoned_reader_retains_roots_heap_and_exclusion() {
    support::check(2, || {
        let registry = Registry::new();
        let record = registry.record();
        let reader = Registry::admit(&registry, &record, false).unwrap();
        let peer_registry = Arc::clone(&registry);
        let peer_record = Arc::clone(&record);
        let peer = thread::spawn(move || peer_registry.try_resume(&peer_record));
        assert_eq!(reader.read(), record.root_checksum);
        drop(reader);
        assert_eq!(peer.join().unwrap(), Attempt::Outstanding);
        assert_eq!(registry.try_resume(&record), Attempt::Outstanding);
        assert_eq!(record.readers.load(Ordering::Acquire), 0);
        assert_eq!(record.abandoned.load(Ordering::Acquire), 1);
        registry.assert_retained(&record);
        // No finish_heap: an unfinished reader is deliberate retention.
    });
}

#[test]
fn busy_retry_preserves_exact_publication() {
    support::check(2, || {
        let registry = Registry::new();
        let record = registry.record();
        let guard = registry.slot.lock().unwrap();
        let peer_registry = Arc::clone(&registry);
        let peer_record = Arc::clone(&record);
        // The fixture holds the lock while a nonblocking attempt completes.
        let peer = thread::spawn(move || peer_registry.try_resume(&peer_record));
        assert_eq!(peer.join().unwrap(), Attempt::Busy);
        assert!(Arc::ptr_eq(&guard.record, &record));
        assert_eq!(guard.state, State::Live);
        drop(guard);
        registry.assert_retained(&record);
        // Same-thread retry succeeds; Busy never consumes the lease.
        assert_eq!(registry.try_resume(&record), Attempt::Retired);
        registry.finish_heap();
    });
}

#[test]
fn renewed_generation_refuses_stale_admission_and_retirement() {
    support::check(2, || {
        let registry = Registry::new();
        let stale = registry.record();
        assert_eq!(registry.try_resume(&stale), Attempt::Retired);
        let current = registry.renew();
        assert_ne!(stale.key.generation, current.key.generation);
        let peer_registry = Arc::clone(&registry);
        let peer_stale = Arc::clone(&stale);
        let peer = thread::spawn(move || {
            assert!(Registry::admit(&peer_registry, &peer_stale, false).is_none());
            assert_eq!(peer_registry.try_resume(&peer_stale), Attempt::Stale);
        });
        let reader = Registry::admit(&registry, &current, false).unwrap();
        assert_eq!(reader.read(), current.root_checksum);
        reader.finish();
        peer.join().unwrap();
        assert_eq!(registry.try_resume(&current), Attempt::Retired);
        registry.finish_heap();
    });
}

#[test]
fn equal_key_with_different_control_is_refused() {
    support::check(1, || {
        let registry = Registry::new();
        let record = registry.record();
        let impostor = Arc::new(Record::new(record.key.generation));
        assert_eq!(impostor.key, record.key);
        assert!(Registry::admit(&registry, &impostor, false).is_none());
        assert_eq!(registry.try_resume(&impostor), Attempt::Stale);
        registry.assert_retained(&record);
        assert_eq!(registry.try_resume(&record), Attempt::Retired);
    });
}

#[test]
fn negative_resume_before_last_read_changes_observed_heap_word() {
    support::negative(2, "collector read raced with resumed mutator", || {
        let registry = Registry::new();
        let record = registry.record();
        let reader = Registry::admit(&registry, &record, false).unwrap();
        let peer_registry = Arc::clone(&registry);
        let peer = thread::spawn(move || peer_registry.resume_ignoring_obligations());
        peer.join().unwrap();
        // Independent object contents detect the broken authority, rather
        // than merely asserting that the counter happens to be nonzero.
        reader.read();
        reader.finish();
    });
}

#[test]
fn negative_ignored_abandonment_loses_required_root() {
    support::negative(1, "unfinished reader lost rooted publication", || {
        let registry = Registry::new();
        let record = registry.record();
        let reader = Registry::admit(&registry, &record, false).unwrap();
        assert_eq!(reader.read(), record.root_checksum);
        drop(reader);
        assert_eq!(record.readers.load(Ordering::Acquire), 0);
        // Broken code considers only active readers, discarding the retained
        // root despite an unfinished collector obligation.
        registry.resume_ignoring_obligations();
        registry.assert_retained(&record);
    });
}

#[test]
fn negative_index_only_reader_decodes_wrong_generation() {
    support::negative(1, "stale reader decoded another generation", || {
        let registry = Registry::new();
        let stale = registry.record();
        assert_eq!(registry.try_resume(&stale), Attempt::Retired);
        registry.renew();
        let reader = Registry::admit_ignoring_generation(&registry, &stale);
        reader.read();
        reader.finish();
    });
}
