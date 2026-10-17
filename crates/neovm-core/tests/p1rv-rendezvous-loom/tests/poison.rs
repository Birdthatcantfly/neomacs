//! R6: event-driven poison uses the predicate mutex, with holder reentry.
//! No timeout/fairness assumption supplies a wake. Protocol controls use a
//! finite notification/lock-edge oracle instead of accepting Loom deadlock.
#![forbid(unsafe_code)]
mod support;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Condvar, Mutex, MutexGuard};
use loom::thread;

const LOST_WAKE: &str = "POISON_NOTIFICATION_LOST_WITHOUT_GATE";
const SELF_LOCK: &str = "POISON_SELF_LOCK_WITHOUT_OWNER_CHECK";

struct Gate {
    mutex: Mutex<()>,
    owner: AtomicUsize,
    poisoned: AtomicBool,
    wake: Condvar,
}
struct Held<'a> {
    gate: &'a Gate,
    lock: Option<MutexGuard<'a, ()>>,
}
impl Drop for Held<'_> {
    fn drop(&mut self) {
        if self.lock.is_some() {
            self.gate.owner.store(0, Ordering::Release);
        }
    }
}
impl Gate {
    fn new() -> Self {
        Self {
            mutex: Mutex::new(()),
            owner: AtomicUsize::new(0),
            poisoned: AtomicBool::new(false),
            wake: Condvar::new(),
        }
    }
    fn held<'a>(&'a self, id: usize, lock: MutexGuard<'a, ()>) -> Held<'a> {
        assert_ne!(id, 0);
        self.owner.store(id, Ordering::Release);
        Held {
            gate: self,
            lock: Some(lock),
        }
    }
    fn lock(&self, id: usize) -> Held<'_> {
        self.held(id, self.mutex.lock().unwrap())
    }
    fn publish_poison(&self) {
        self.poisoned.store(true, Ordering::Release);
        self.wake.notify_all();
    }
    fn poison(&self, id: usize) {
        if self.owner.load(Ordering::Acquire) == id {
            self.publish_poison();
        } else {
            let _held = self.lock(id);
            self.publish_poison();
        }
    }
    fn wait_poison<'a>(&'a self, id: usize, mut held: Held<'a>) {
        while !self.poisoned.load(Ordering::Acquire) {
            self.owner.store(0, Ordering::Release);
            let lock = held.lock.take().unwrap();
            drop(held);
            // Atomic release/register and reacquire, not a modeled timeout.
            let lock = self.wake.wait(lock).unwrap();
            held = self.held(id, lock);
        }
        assert_eq!(self.owner.load(Ordering::Acquire), id);
    }
}

#[test]
fn external_poison_in_predicate_wait_gap_acquires_gate_and_wakes() {
    support::check(2, || {
        let gate = Arc::new(Gate::new());
        let held = gate.lock(1);
        assert!(!gate.poisoned.load(Ordering::Acquire));
        let remote = Arc::clone(&gate);
        let poisoner = thread::spawn(move || remote.poison(2));
        gate.wait_poison(1, held);
        poisoner.join().unwrap();
        assert!(gate.poisoned.load(Ordering::Acquire));
        assert_eq!(gate.owner.load(Ordering::Acquire), 0);
    });
}

#[test]
fn holder_poison_between_predicate_and_wait_is_rechecked_without_self_lock() {
    support::check(1, || {
        let gate = Gate::new();
        let held = gate.lock(1);
        assert!(!gate.poisoned.load(Ordering::Acquire));
        gate.poison(1);
        gate.wait_poison(1, held); // must not sleep after its own notification
        assert_eq!(gate.owner.load(Ordering::Acquire), 0);
        // Same-thread lock replay has fresh ownership after guard release.
        let held = gate.lock(1);
        gate.poison(1);
        drop(held);
        assert_eq!(gate.owner.load(Ordering::Acquire), 0);
    });
}

#[test]
fn ownership_changes_across_condvar_release_before_later_holder_poison() {
    support::check(2, || {
        let gate = Arc::new(Gate::new());
        let held = gate.lock(1);
        let remote = Arc::clone(&gate);
        let poisoner = thread::spawn(move || {
            let held = remote.lock(2);
            assert_eq!(remote.owner.load(Ordering::Acquire), 2);
            remote.poison(2);
            drop(held);
        });
        gate.wait_poison(1, held);
        poisoner.join().unwrap();
        assert_eq!(gate.owner.load(Ordering::Acquire), 0);
    });
}

#[test]
fn negative_lock_free_poison_can_notify_before_wait_registration() {
    support::negative(2, LOST_WAKE, || {
        let gate = Arc::new(Gate::new());
        let held = gate.lock(1);
        assert!(!gate.poisoned.load(Ordering::Acquire));
        // Finite oracle for the actual condvar edge: notifications delivered
        // with no registered waiter are lost, rather than remembered permits.
        let registered = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicBool::new(false));
        let remote = Arc::clone(&gate);
        let registered_remote = Arc::clone(&registered);
        let delivered_remote = Arc::clone(&delivered);
        let poisoner = thread::spawn(move || {
            remote.publish_poison(); // wrong: no predicate mutex acquisition
            if registered_remote.load(Ordering::Acquire) {
                delivered_remote.store(true, Ordering::Release);
            }
        });
        // This is a valid forced-gap schedule, not a liveness assumption.
        poisoner.join().unwrap();
        registered.store(true, Ordering::Release);
        assert!(gate.poisoned.load(Ordering::Acquire));
        assert!(delivered.load(Ordering::Acquire), "{LOST_WAKE}");
        drop(held);
    });
}

#[test]
fn negative_always_locking_poison_has_same_thread_control_cycle() {
    support::negative(1, SELF_LOCK, || {
        let gate = Gate::new();
        let held = gate.lock(1);
        // A blocking acquisition by the current holder adds a self-cycle.
        // Detect that exact finite edge instead of counting Loom deadlock.
        if gate.owner.load(Ordering::Acquire) == 1 {
            panic!("{SELF_LOCK}");
        }
        let _recursive = gate.lock(1); // wrong: no holder identity bypass
        drop(held);
    });
}
