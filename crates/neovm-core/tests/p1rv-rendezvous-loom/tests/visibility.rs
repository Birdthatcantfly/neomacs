#![forbid(unsafe_code)]

mod support;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;

const EPOCH: usize = 7;
const ROOTS: usize = 31;
const LOGS: usize = 47;

/// Payload atomics stand in for immutable roots and the stopped GC image.
/// Relaxed accesses let the negative controls expose stale data without unsafe
/// memory or a data race that would mask the intended assertion witness.
struct SafepointImage {
    roots: AtomicUsize,
    logs: AtomicUsize,
    ack: AtomicUsize,
}

fn publication_case(
    publication: Ordering,
    observation: Ordering,
    ack_before_payload: bool,
    witness: &'static str,
) -> bool {
    let image = Arc::new(SafepointImage {
        roots: AtomicUsize::new(0),
        logs: AtomicUsize::new(0),
        ack: AtomicUsize::new(0),
    });
    let owner_image = Arc::clone(&image);
    let owner = thread::spawn(move || {
        if ack_before_payload {
            owner_image.ack.store(EPOCH, publication);
        }
        owner_image.roots.store(ROOTS, Ordering::Relaxed);
        owner_image.logs.store(LOGS, Ordering::Relaxed);
        if !ack_before_payload {
            owner_image.ack.store(EPOCH, publication);
        }
    });

    // This is deliberately a one-shot keeper scan: missing the ACK is allowed.
    // There is no mutex or join before this observation that could supply an
    // accidental happens-before edge in place of publication/observation.
    let observed = image.ack.load(observation) == EPOCH;
    if observed {
        let roots = image.roots.load(Ordering::Relaxed);
        let logs = image.logs.load(Ordering::Relaxed);
        assert!(roots == ROOTS && logs == LOGS, "{witness}");
    }
    owner.join().unwrap();
    observed
}

#[test]
fn release_ack_acquire_scan_exposes_roots_and_logs() {
    // Unmodeled coverage is accumulated across schedules, never read by the
    // modeled participants, and cannot synchronize their payload accesses.
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let coverage = std::sync::Arc::clone(&observed);
    support::check(2, move || {
        if publication_case(
            Ordering::Release,
            Ordering::Acquire,
            false,
            "acquired ACK exposed incomplete roots or logs",
        ) {
            coverage.fetch_add(1, Ordering::Relaxed);
        }
    });
    let observed = observed.load(Ordering::Relaxed);
    assert!(
        observed > 0,
        "positive model never observed a safepoint ACK"
    );
    eprintln!("schedules observing a safepoint ACK: {observed}");
}

#[test]
fn relaxed_ack_publication_has_a_stale_payload_witness() {
    const WITNESS: &str = "relaxed ACK publication exposed stale roots or logs";
    support::negative(2, WITNESS, || {
        publication_case(Ordering::Relaxed, Ordering::Acquire, false, WITNESS);
    });
}

#[test]
fn relaxed_ack_observation_has_a_stale_payload_witness() {
    const WITNESS: &str = "relaxed ACK observation exposed stale roots or logs";
    support::negative(2, WITNESS, || {
        publication_case(Ordering::Release, Ordering::Relaxed, false, WITNESS);
    });
}

#[test]
fn ack_before_payload_has_an_incomplete_roots_witness() {
    const WITNESS: &str = "ACK before publication exposed incomplete roots or logs";
    support::negative(2, WITNESS, || {
        publication_case(Ordering::Release, Ordering::Acquire, true, WITNESS);
    });
}

/// The collector's response must be visible before the owner can run again.
/// These fields model collector-written allocation/age state and returned log
/// generation; no collector or owner mutex participates in this experiment.
struct ResumeImage {
    allocation_state: AtomicUsize,
    log_generation: AtomicUsize,
    completed_epoch: AtomicUsize,
}

fn resume_case(publication: Ordering, observation: Ordering, witness: &'static str) -> bool {
    let image = Arc::new(ResumeImage {
        allocation_state: AtomicUsize::new(0),
        log_generation: AtomicUsize::new(0),
        completed_epoch: AtomicUsize::new(0),
    });
    let collector_image = Arc::clone(&image);
    let collector = thread::spawn(move || {
        collector_image
            .allocation_state
            .store(ROOTS, Ordering::Relaxed);
        collector_image
            .log_generation
            .store(LOGS, Ordering::Relaxed);
        collector_image.completed_epoch.store(EPOCH, publication);
    });

    // Observe before joining: the completion edge alone must carry GC writes.
    let observed = image.completed_epoch.load(observation) == EPOCH;
    if observed {
        let allocation_state = image.allocation_state.load(Ordering::Relaxed);
        let log_generation = image.log_generation.load(Ordering::Relaxed);
        assert!(
            allocation_state == ROOTS && log_generation == LOGS,
            "{witness}"
        );
    }
    collector.join().unwrap();
    observed
}

#[test]
fn release_completion_acquire_resume_exposes_collector_writes() {
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let coverage = std::sync::Arc::clone(&observed);
    support::check(2, move || {
        if resume_case(
            Ordering::Release,
            Ordering::Acquire,
            "acquired completion exposed incomplete collector writes",
        ) {
            coverage.fetch_add(1, Ordering::Relaxed);
        }
    });
    let observed = observed.load(Ordering::Relaxed);
    assert!(observed > 0, "positive model never observed completion");
    eprintln!("schedules observing collector completion: {observed}");
}

#[test]
fn relaxed_completion_publication_has_a_stale_resume_witness() {
    const WITNESS: &str = "relaxed completion publication exposed stale collector writes";
    support::negative(2, WITNESS, || {
        resume_case(Ordering::Relaxed, Ordering::Acquire, WITNESS);
    });
}

#[test]
fn relaxed_resume_observation_has_a_stale_resume_witness() {
    const WITNESS: &str = "relaxed resume observation exposed stale collector writes";
    support::negative(2, WITNESS, || {
        resume_case(Ordering::Release, Ordering::Relaxed, WITNESS);
    });
}
