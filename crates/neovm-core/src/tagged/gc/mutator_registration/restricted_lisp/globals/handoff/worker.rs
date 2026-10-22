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
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Snapshot, ExecError> {
            frame = Some(session.seed(&mut owner, index, nil)?);
            let frame = frame.as_mut().ok_or(ExecError::Slot)?;
            events.send(Event::Seeded).expect("keeper alive at seed");
            for _ in 0..PHASES {
                assert_eq!(receive(&commands), Command::Advance);
                pause(session, &mut owner, frame)?;
                let before = snapshot(&mut owner, frame)?;
                owner.enter_native(&[])?;
                events
                    .send(Event::Paused(before))
                    .expect("keeper alive at pause");
                assert_eq!(receive(&commands), Command::Resume);
                owner.leave_native()?;
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
        if owner.local.is_none() {
            owner
                .leave_native()
                .map_err(|e| format!("restore for cleanup: {e:?}"))?;
        }
        with_run(&mut owner, |run| {
            run.clear_root_slots()?;
            Ok(())
        })
        .map_err(|e| format!("clear census: {e:?}"))?;
        owner
            .retire_rooted(&[])
            .map_err(|e| format!("retire: {e:?}"))
    })();
    let _ = events.send(Event::Finished { result, cleanup });
}
