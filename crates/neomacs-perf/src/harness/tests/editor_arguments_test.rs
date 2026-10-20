//! Command/provenance tests only: no editor, PTY, package fetch or counter process.
use super::*;
use std::ffi::OsString;

pub(super) fn test_editor() -> EditorProvenance {
    EditorProvenance {
        path: "/explicit artifact/neomacs".to_owned(),
        executable_sha256: "test-editor-sha256".to_owned(),
        executable_size_bytes: 42,
        pdump_fingerprint: "test-build-fingerprint".to_owned(),
        portable_dump: crate::PortableDumpProvenance::Loaded {
            path: "/explicit artifact/neomacs.pdump".to_owned(),
            sha256: "test-pdump-sha256".to_owned(),
            size_bytes: 24,
        },
        version: "test-only".to_owned(),
        kind: EditorKind::Neomacs,
        capabilities: EditorCapabilities {
            native_compilation: false,
            tree_sitter: true,
            dynamic_modules: true,
            video_playback: false,
            webview: false,
            embedded_terminal: false,
        },
    }
}

pub(super) fn test_host() -> HostProvenance {
    HostProvenance {
        operating_system: "test-only".to_owned(),
        architecture: "test-only".to_owned(),
        kernel_release: None,
        cpu_model: None,
        logical_cpu_count: 1,
        allowed_cpus: None,
        selected_cpu: None,
        scaling_governor: None,
        perf_event_paranoid: None,
        continuous_integration: false,
    }
}

fn prepared(root: &Path) -> PreparedScenario {
    PreparedScenario {
        fixture: root.join("fixture with spaces.el"),
        provenance: root.join("input-provenance.json"),
        result: root.join("result.json"),
        sentinel: root.join("completed"),
        terminal_bytes: root.join("terminal.ansi"),
        gui_app_log: root.join("gui-app.log"),
        gui_weston_log: root.join("weston.log"),
        gui_runtime_directory: root.join("gui-runtime"),
        sandbox: MelpaSandbox::new("perf-editor-arguments-command-only")
            .expect("create private command-test environment"),
        workload: PreparedWorkload::EditorWorkload {
            source: root.join("source.el"),
            repository: None,
            startup: Some(root.join("package startup.el")),
            packages: None,
        },
    }
}

#[test]
fn editor_arguments_reach_actual_frontend_commands_verbatim_before_loads() {
    let root = crate::workspace_root().join("tmp/editor-argument-command-only");
    let prepared = prepared(&root);
    let editor = root.join("real artifact/neomacs");
    let policy = "--rows-query-admission=exact-proven";
    let literal = "space = unicode λ; $(literal)";
    let arguments = vec![policy.to_owned(), "--eval".to_owned(), literal.to_owned()];
    for frontend in [
        Frontend::Batch,
        Frontend::Tui {
            rows: 40,
            columns: 120,
        },
        Frontend::Gui {
            width: 1200,
            height: 800,
        },
    ] {
        for cpu in [None, Some(4)] {
            let request = RunRequest::new(
                ScenarioId::SustainedEditing,
                &editor,
                NonZeroU32::new(1).unwrap(),
            )
            .with_frontend(frontend)
            .with_machine_policy(MachinePolicy {
                cpu,
                required_governor: None,
            })
            .with_inherited_environment(BTreeMap::new())
            .with_editor_arguments(arguments.clone());
            assert!(
                request.benchmark_environment().is_empty(),
                "argv must not become environment overrides"
            );
            let command = frontend_command(&request, &root, &prepared, None);
            let actual: Vec<OsString> = command.get_args().map(OsString::from).collect();
            let prefix: Vec<OsString> = match frontend {
                Frontend::Batch => {
                    assert_eq!(
                        command.get_program(),
                        if cpu.is_some() {
                            std::ffi::OsStr::new("taskset")
                        } else {
                            editor.as_os_str()
                        }
                    );
                    let mut prefix = if cpu.is_some() {
                        vec!["-c".into(), "4".into(), editor.as_os_str().to_owned()]
                    } else {
                        Vec::new()
                    };
                    prefix.extend(["--batch".into(), "-Q".into()]);
                    prefix
                }
                Frontend::Tui { .. } => {
                    assert_eq!(command.get_program(), "python3");
                    vec![
                        root.join("tools/bench/pty-run.py").into_os_string(),
                        editor.as_os_str().to_owned(),
                        "-nw".into(),
                        "-Q".into(),
                    ]
                }
                Frontend::Gui { .. } => {
                    assert_eq!(
                        command.get_program(),
                        if cpu.is_some() {
                            std::ffi::OsStr::new("taskset")
                        } else {
                            editor.as_os_str()
                        }
                    );
                    let mut prefix = if cpu.is_some() {
                        vec!["-c".into(), "4".into(), editor.as_os_str().to_owned()]
                    } else {
                        Vec::new()
                    };
                    prefix.push("-Q".into());
                    prefix
                }
            };
            assert_eq!(&actual[..prefix.len()], prefix.as_slice());
            let expected: Vec<OsString> = arguments
                .iter()
                .map(|argument| OsString::from(argument.as_str()))
                .collect();
            assert_eq!(
                &actual[prefix.len()..prefix.len() + expected.len()],
                expected.as_slice()
            );
            assert_eq!(
                actual
                    .iter()
                    .filter(|arg| arg.as_os_str() == policy)
                    .count(),
                1
            );
            // The actual frontend function appends pdump capture before workload
            // startup and fixture; all explicit editor arguments must precede it.
            assert_eq!(actual[prefix.len() + expected.len()], "--eval");
            assert_eq!(
                &actual[actual.len() - 4..],
                &[
                    OsString::from("--load"),
                    root.join("package startup.el").into_os_string(),
                    OsString::from("--load"),
                    root.join("fixture with spaces.el").into_os_string(),
                ]
            );
        }
    }
}

#[test]
fn editor_arguments_default_empty_keeps_the_original_frontend_argv() {
    let root = crate::workspace_root().join("tmp/editor-argument-default-command-only");
    let prepared = prepared(&root);
    let request = RunRequest::new(
        ScenarioId::SustainedEditing,
        root.join("neomacs"),
        NonZeroU32::new(1).unwrap(),
    )
    .with_frontend(Frontend::Batch);
    assert!(request.editor_arguments().is_empty());
    let command = frontend_command(&request, &root, &prepared, None);
    let actual: Vec<OsString> = command.get_args().map(OsString::from).collect();
    assert_eq!(
        &actual[..3],
        &[
            OsString::from("--batch"),
            OsString::from("-Q"),
            OsString::from("--eval")
        ]
    );
    assert!(
        !actual
            .iter()
            .any(|arg| arg.to_string_lossy().contains("rows-query-admission"))
    );
}
