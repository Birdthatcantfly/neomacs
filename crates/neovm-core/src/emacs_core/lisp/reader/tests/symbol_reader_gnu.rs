//! GNU 31.1 reader/symbol regression fixtures, refreshed only from GNU.
use crate::test_utils::runtime_startup_eval_one;
use std::path::PathBuf;
fn assert_gnu(name: &str, form: &str, fixture: &str) {
    crate::test_utils::init_test_tracing();
    let expected = if std::env::var("UPDATE_EXPECT").as_deref() == Ok("1") {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("../../tmp")
            .join(format!("symbols-{name}-{}.el", std::process::id()));
        std::fs::write(&script, format!("(prin1 {form})")).unwrap();
        let emacs = std::env::var_os("EMACS").unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap())
                .join(".local/bin/emacs")
                .into_os_string()
        });
        let sandbox = std::env::var_os("NEOVM_LISP_SANDBOX").unwrap_or_else(|| {
            "/home/exec/Projects/github.com/eval-exec/neomacs-main/tmp/v10x/sandbox-run.sh".into()
        });
        let output = std::process::Command::new(sandbox)
            .arg(emacs)
            .args(["-Q", "--batch", "-l"])
            .arg(script)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = format!("OK {}", String::from_utf8(output.stdout).unwrap());
        let path = root
            .join("src/emacs_core/lisp/reader/tests/symbol_reader_gnu")
            .join(format!("{name}.expect"));
        let saved = format!("{text}\n");
        if std::fs::read_to_string(&path).ok().as_deref() != Some(saved.as_str()) {
            std::fs::write(path, saved).unwrap();
        }
        text
    } else {
        fixture.trim_end().to_owned()
    };
    assert_eq!(runtime_startup_eval_one(form), expected, "{name}");
}

#[test]
fn symbols_gnu_identity() {
    assert_gnu(
        "identity",
        r###"(list (intern-soft (make-symbol "car")) (intern-soft 'car) (let ((ob (obarray-make))) (intern-soft (intern "foo" ob))) (let ((ob (obarray-make))) (intern "foo" ob) (intern-soft 'foo ob)))"###,
        include_str!("symbol_reader_gnu/identity.expect"),
    );
}
