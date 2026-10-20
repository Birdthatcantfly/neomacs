//! C3 operation histories. No engine dependency or payload lock: ordinary
//! checked cells synchronize only through the reviewed counted-reader word.
mod model;
mod support;

use loom::sync::Arc;
use loom::thread;
use model::{Authority, Checked};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Snapshot {
    value: Option<usize>, // None is void; Some(0) stands for Lisp nil.
    version: usize,
}

#[derive(Clone, Copy)]
struct History {
    states: [Snapshot; 3],
    length: usize,
}

struct Global {
    authority: Authority,
    cell: Checked<Snapshot>,
    // Writer-only audit, never a synchronization source for readers.
    history: Checked<History>,
}

impl Global {
    fn new() -> Self {
        let initial = Snapshot {
            value: Some(0),
            version: 0,
        };
        Self {
            authority: Authority::new(),
            cell: Checked::new(initial),
            history: Checked::new(History {
                states: [initial; 3],
                length: 1,
            }),
        }
    }

    fn read(&self) -> Option<Snapshot> {
        let _reader = self.authority.read()?;
        // The owned copy models installation into the owner's canonical root
        // before this reader is dropped; it carries no cell reference.
        Some(self.cell.get())
    }

    fn write(&self, value: Option<usize>) -> Option<Option<usize>> {
        let _writer = self.authority.write()?;
        let mut history = self.history.get();
        let next = Snapshot {
            value,
            version: history.length,
        };
        self.cell.set(next);
        history.states[history.length] = next;
        history.length += 1;
        self.history.set(history);
        // set returns its assigned value; makunbound's symbol result is an
        // immediate identity and is tested separately by GNU/engine twins.
        Some(value)
    }

    fn audit(&self, reads: &[Option<Snapshot>]) {
        self.authority.assert_quiescent();
        let history = self.history.get();
        assert_eq!(history.length, 3, "both writers eventually committed");
        for read in reads.iter().flatten() {
            assert!(
                history.states[..history.length].contains(read),
                "read has no legal symbol history"
            );
        }
        assert_eq!(self.cell.get(), history.states[2]);
    }
}

fn histories(owners: usize, second: Option<usize>) {
    support::check(owners, move || {
        let global = Arc::new(Global::new());
        let a = Arc::clone(&global);
        let writer_a = thread::spawn(move || a.write(Some(11)));
        if owners == 2 {
            let b = global.write(second);
            let a = writer_a.join().unwrap();
            if a.is_none() {
                assert_eq!(global.write(Some(11)), Some(Some(11)));
            }
            if b.is_none() {
                assert_eq!(global.write(second), Some(second));
            }
            global.audit(&[global.read()]);
            return;
        }
        let b = Arc::clone(&global);
        let writer_b = thread::spawn(move || b.write(second));
        let extra = (owners == 4).then(|| {
            let reader = Arc::clone(&global);
            thread::spawn(move || [reader.read(), reader.read()])
        });
        let reads = [global.read(), global.read()];
        let a = writer_a.join().unwrap();
        let b = writer_b.join().unwrap();
        let extra = extra.map(|reader| reader.join().unwrap());
        // Finite initial attempts may refuse. Retry after joins is an explicit
        // serial suffix, not an unbounded spin or an assumption of fairness.
        if a.is_none() {
            assert_eq!(global.write(Some(11)), Some(Some(11)));
        }
        if b.is_none() {
            assert_eq!(global.write(second), Some(second));
        }
        global.audit(&reads);
        if let Some(extra) = extra {
            global.audit(&extra);
        }
    });
}

#[test]
fn single_owner_distinguishes_nil_and_void() {
    support::exhaustive(1, || {
        let global = Global::new();
        assert_eq!(global.read().unwrap().value, Some(0));
        assert_eq!(global.write(None), Some(None));
        assert_eq!(global.read().unwrap().value, None);
        assert_eq!(global.write(Some(73)), Some(Some(73)));
        assert_eq!(global.read().unwrap().value, Some(73));
        global.audit(&[global.read()]);
    });
}

#[test]
fn two_owner_writer_history() {
    histories(2, Some(22));
}

#[test]
fn three_owner_reader_and_two_writers() {
    histories(3, Some(22));
}

#[test]
fn four_owner_readers_and_two_writers() {
    histories(4, Some(22));
}

#[test]
fn set_vs_makunbound_has_a_legal_bound_value_pair() {
    histories(3, None);
}

#[test]
fn set_answer_remains_its_own_value_after_another_writer() {
    support::check(2, || {
        let global = Arc::new(Global::new());
        let assigned = global.write(Some(11)).unwrap();
        let b = Arc::clone(&global);
        thread::spawn(move || assert_eq!(b.write(Some(22)), Some(Some(22))))
            .join()
            .unwrap();
        assert_eq!(assigned, Some(11));
        assert_eq!(global.read().unwrap().value, Some(22));
        global.audit(&[global.read()]);
    });
}

#[test]
fn read_answer_root_survives_overwrite_and_stop() {
    support::check(2, || {
        let global = Arc::new(Global::new());
        assert_eq!(global.write(Some(41)), Some(Some(41)));
        let reader = Arc::clone(&global);
        let answer = thread::spawn(move || reader.read().unwrap())
            .join()
            .unwrap();
        assert_eq!(global.write(None), Some(None));
        global.authority.assert_quiescent();
        // Joined actors stand for the keeper's matching stop. A root ledger,
        // not a moving allocator: only B's installed answer retains object 41.
        let roots = [global.cell.get().value, answer.value];
        assert!(roots.contains(&Some(41)), "rooted global answer reclaimed");
        global.audit(&[Some(answer)]);
    });
}

/// Finite retries preserve Busy as a legal outcome. No try-acquisition is
/// unwrapped, and no successful commit may disappear from the history count.
fn bounded_write(global: &Global, value: usize) -> (bool, usize) {
    let mut refused = 0;
    for _ in 0..3 {
        if let Some(answer) = global.write(Some(value)) {
            assert_eq!(answer, Some(value));
            return (true, refused);
        }
        refused += 1;
        thread::yield_now();
    }
    (false, refused)
}

#[test]
fn completed_writer_history_never_loses_a_commit() {
    support::check(2, || {
        let global = Arc::new(Global::new());
        let g = Arc::clone(&global);
        let other = thread::spawn(move || bounded_write(&g, 11));
        let (first_done, first_refused) = bounded_write(&global, 22);
        let (other_done, other_refused) = other.join().unwrap();
        let completed = usize::from(first_done) + usize::from(other_done);
        let history = global.history.get();
        let current = global.cell.get();
        global.authority.assert_quiescent();
        let state = format!(
            "completed={completed} refused={first_refused}/{other_refused} \
             states={:?} current={current:?}",
            &history.states[..history.length]
        );
        // The writer's version increment and cell publication are one guarded
        // primitive. A lost increment HERE is a protocol error, regardless of
        // the legal Busy branches. This is not a Lisp read/compute/set compound.
        assert_eq!(history.length, completed + 1, "lost writer commit: {state}");
        assert_eq!(
            current.version, completed,
            "lost version increment: {state}"
        );
        assert_eq!(
            current, history.states[completed],
            "lost final write: {state}"
        );
        for (done, value) in [(first_done, 22), (other_done, 11)] {
            assert_eq!(
                history.states[1..history.length]
                    .iter()
                    .filter(|entry| entry.value == Some(value))
                    .count(),
                usize::from(done),
                "writer history disagrees with completion: {state}"
            );
        }
    });
}

#[test]
fn set_reloading_answer_after_release_is_rejected() {
    support::negative(2, "set returned another owner's value", || {
        let global = Arc::new(Global::new());
        let assigned = global.write(Some(11)).unwrap();
        let b = Arc::clone(&global);
        thread::spawn(move || assert_eq!(b.write(Some(22)), Some(Some(22))))
            .join()
            .unwrap();
        // Broken set reloads the backing instead of rooting its own operand.
        let returned = global.read().unwrap().value;
        assert!(returned == assigned, "set returned another owner's value");
    });
}

#[test]
fn read_releasing_before_root_install_is_rejected() {
    support::negative(2, "symbol answer escaped before rooting", || {
        let global = Arc::new(Global::new());
        assert_eq!(global.write(Some(41)), Some(Some(41)));
        let g = Arc::clone(&global);
        let unrooted = thread::spawn(move || g.read().unwrap()).join().unwrap();
        assert_eq!(global.write(None), Some(None));
        global.authority.assert_quiescent();
        // Deliberately omit the answer home from stopped roots. The checked
        // ledger detects death before any real freed pointer is dereferenced.
        let roots = [global.cell.get().value];
        assert!(
            roots.contains(&unrooted.value),
            "symbol answer escaped before rooting"
        );
    });
}
