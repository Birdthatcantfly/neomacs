//! Fresh subprocesses supply startup values; no test mutates its environment.
use super::*;
use std::ffi::OsString;
use std::process::Command;
use std::sync::atomic::Ordering;

const WORKER_MODE: &str = "NEOMACS_LAYOUT_STATS_TEST_WORKER";
const EXPECTED: &str = "NEOMACS_LAYOUT_STATS_TEST_EXPECTED";
const WORKER_TEST: &str = "engine::diagnostic_knobs::tests::process_startup_worker";

#[test]
fn layout_stats_startup_path_keeps_unicode_and_empty_semantics() {
    let cases: Vec<(Option<OsString>, Option<&str>)> = vec![
        (None, None),
        (Some(OsString::from("")), None),
        (Some(OsString::from("0")), Some("0")),
        (Some(OsString::from("1")), Some("1")),
        (Some(OsString::from(" ")), Some(" ")),
        (
            Some(OsString::from("/diagnostics/layout-λ.log")),
            Some("/diagnostics/layout-λ.log"),
        ),
    ];
    #[cfg(unix)]
    let cases = {
        use std::os::unix::ffi::OsStringExt;
        let mut cases = cases;
        cases.push((Some(OsString::from_vec(vec![0xff])), None));
        cases
    };
    for (value, expected) in cases {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .arg("--exact")
            .arg(WORKER_TEST)
            .arg("--test-threads=1")
            .env(WORKER_MODE, "1")
            .env(EXPECTED, expected.unwrap_or(""))
            .env_remove("NEOMACS_LAYOUT_STATS_FILE");
        if let Some(value) = value.as_ref() {
            command.env("NEOMACS_LAYOUT_STATS_FILE", value);
        }
        let output = command.output().expect("isolated diagnostic worker");
        assert!(
            output.status.success(),
            "startup path {value:?}: status={:?} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "worker filter must execute exactly one test: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn process_startup_worker() {
    if std::env::var_os(WORKER_MODE).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    let expected = std::env::var(EXPECTED).expect("worker expectation");
    let expected = (!expected.is_empty()).then_some(expected);
    assert_eq!(ENV_READS.load(Ordering::Relaxed), 0);
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let expected = expected.clone();
            std::thread::spawn(move || {
                let mut first = None;
                for _ in 0..64 {
                    let actual = layout_stats_path();
                    assert_eq!(actual, expected.as_deref());
                    if let Some(path) = actual {
                        let identity = (path.as_ptr() as usize, path.len());
                        if let Some(first) = first {
                            assert_eq!(
                                identity, first,
                                "path must borrow its published allocation"
                            );
                        }
                        first = Some(identity);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("diagnostic reader worker");
    }
    assert_eq!(ENV_READS.load(Ordering::Relaxed), 1);
}
