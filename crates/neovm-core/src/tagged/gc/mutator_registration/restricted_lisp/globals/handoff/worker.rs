//! Own each frame/token until cleanup, independently of execution's outcome.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub(super) words: Vec<usize>,
    pub(super) owned: Vec<usize>,
    pub(super) points: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    Advance,
    Resume,
    Finish,
}

#[derive(Debug)]
pub(super) enum Event {
    Seeded,
    Paused(Snapshot),
    Resumed(Snapshot),
    Finished {
        result: Result<Snapshot, String>,
        cleanup: Result<(), String>,
    },
}

pub(super) struct Actor {
    pub(super) commands: Sender<Command>,
    pub(super) events: Receiver<Event>,
}

pub(super) fn receive<T>(rx: &Receiver<T>) -> T {
    #[cfg(not(miri))]
    {
        rx.recv_timeout(CHANNEL_WAIT)
            .expect("C3 handoff channel progress")
    }
    // Miri covers the tiny protocol, without a wall-clock deadline assertion.
    #[cfg(miri)]
    {
        rx.recv().expect("C3 handoff channel progress")
    }
}

fn snapshot<'era>(owner: &mut Mutator<'era>, frame: &Frame<'era>) -> Result<Snapshot, ExecError> {
    with_run(owner, |run| {
        let mut owned: Vec<_> = frame.machine.owned.iter().copied().collect();
        owned.sort_unstable();
        Ok(Snapshot {
            words: frame
                .slots
                .iter()
                .map(|slot| slot.word_for_test(run))
                .collect(),
            owned,
            points: frame.machine.safepoints,
        })
    })
}

fn pause<'era>(
    session: &GlobalSession,
    owner: &mut Mutator<'era>,
    frame: &mut Frame<'era>,
) -> Result<(), ExecError> {
    loop {
        match session.step(owner, frame)? {
            Step::Safepoint => return Ok(()),
            Step::Retry => thread::yield_now(),
            Step::Progress => {}
            Step::Done => return Err(ExecError::Type),
        }
    }
}

fn native_transition<T>(
    mut transition: impl FnMut() -> Result<T, RendezvousError>,
) -> Result<T, RendezvousError> {
    // Simultaneous restores can refuse before taking an image. Busy leaves
    // ownership with this worker; retry outside every run/entry/image loan.
    // Keep the fixture finite without a wall-clock condition under Miri.
    for _ in 0..STEP_CAP {
        match transition() {
            Err(RendezvousError::Busy) => thread::yield_now(),
            outcome => return outcome,
        }
    }
    Err(RendezvousError::Busy)
}

fn retire_owner(mut owner: Mutator<'_>) -> Result<(), String> {
    for _ in 0..STEP_CAP {
        match owner.retire_rooted(&[]) {
            Ok(()) => return Ok(()),
            Err(failure) if failure.reason == RendezvousError::Busy => {
                owner = failure.into_owner();
                thread::yield_now();
            }
            Err(failure) => return Err(format!("retire: {:?}", failure.reason)),
        }
    }
    Err("retire: Busy retry budget exhausted".to_owned())
}

pub(super) fn run<'era>(
    session: &GlobalSession,
    nil: &SharedRoot,
    permit: MutatorPermit<'era>,
    heap: HeapIdentity,
    index: usize,
    commands: Receiver<Command>,
    events: Sender<Event>,
) {
    let mut owner = activate_owner(permit, heap, 3000 + index as u64);
    let mut frame = None;
    let mut native = false;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Snapshot, ExecError> {
            frame = Some(session.seed(&mut owner, index, nil)?);
            let frame = frame.as_mut().ok_or(ExecError::Slot)?;
            events.send(Event::Seeded).expect("keeper alive at seed");
            for _ in 0..PHASES {
                assert_eq!(receive(&commands), Command::Advance);
                pause(session, &mut owner, frame)?;
                let before = snapshot(&mut owner, frame)?;
                native_transition(|| owner.enter_native(&[]))?;
                native = true;
                events
                    .send(Event::Paused(before))
                    .expect("keeper alive at pause");
                assert_eq!(receive(&commands), Command::Resume);
                native_transition(|| owner.leave_native())?;
                native = false;
                events
                    .send(Event::Resumed(snapshot(&mut owner, frame)?))
                    .expect("keeper alive at resume");
            }
            assert_eq!(receive(&commands), Command::Finish);
            assert_eq!(session.step(&mut owner, frame)?, Step::Done);
            snapshot(&mut owner, frame)
        },
    ));
    let result = match outcome {
        Ok(result) => result.map_err(|error| format!("owner {index} executor: {error:?}")),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "non-string panic".to_owned());
            Err(format!("owner {index} panic: {message}"))
        }
    };
    drop(frame);
    let cleanup = (|| -> Result<(), String> {
        // A native owner still has a local shell, so local.is_none() cannot
        // distinguish a published image from restored running state.
        if native {
            native_transition(|| owner.leave_native())
                .map_err(|e| format!("restore for cleanup: {e:?}"))?;
        }
        with_run(&mut owner, |run| {
            run.clear_root_slots()?;
            Ok(())
        })
        .map_err(|e| format!("clear census: {e:?}"))?;
        retire_owner(owner)
    })();
    let _ = events.send(Event::Finished { result, cleanup });
}
