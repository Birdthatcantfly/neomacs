use super::*;

fn fixture() -> (tempfile::TempDir, Vec<PathBuf>) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    let sources = vec![root.path().join("a.el"), root.path().join("sub/b.el")];
    for source in &sources {
        fs::write(source, b"(provide 'fixture)").unwrap();
    }
    (root, sources)
}

#[test]
fn wave_children_keep_canonical_outputs_unchanged_until_join_and_validation() {
    let (root, sources) = fixture();
    for source in &sources {
        fs::write(source.with_extension("elc"), b"old").unwrap();
    }
    let wave = PlannedWave::create(root.path(), &sources).unwrap();
    std::thread::scope(|scope| {
        for job in wave.jobs() {
            scope.spawn(move || {
                assert_eq!(fs::read(&job.canonical).unwrap(), b"old");
                fs::write(&job.pending, b"new bytecode").unwrap();
                assert_eq!(fs::read(&job.canonical).unwrap(), b"old");
            });
        }
    });
    wave.validate()
        .unwrap()
        .publish()
        .unwrap()
        .finish()
        .unwrap();
    for source in sources {
        assert_eq!(
            fs::read(source.with_extension("elc")).unwrap(),
            b"new bytecode"
        );
    }
}

#[test]
fn missing_output_retains_pending_products_and_publishes_nothing() {
    let (root, sources) = fixture();
    let wave = PlannedWave::create(root.path(), &sources).unwrap();
    let staging = wave.directory.path().to_path_buf();
    for source in &sources {
        fs::write(source.with_extension("elc"), b"old canonical").unwrap();
    }
    let pending = wave.jobs()[0].pending.clone();
    fs::write(&pending, b"one successful child").unwrap();
    let error = wave.validate().err().unwrap();
    assert!(error.to_string().contains(staging.to_str().unwrap()));
    assert_eq!(fs::read(pending).unwrap(), b"one successful child");
    for source in sources {
        assert_eq!(
            fs::read(source.with_extension("elc")).unwrap(),
            b"old canonical"
        );
    }
}

#[test]
fn wave_rejects_duplicate_and_outside_sources_before_workers() {
    let (root, sources) = fixture();
    assert!(PlannedWave::create(root.path(), &[sources[0].clone(), sources[0].clone()]).is_err());
    let other = tempfile::tempdir().unwrap();
    assert!(PlannedWave::create(other.path(), &sources).is_err());
}

#[test]
fn redirection_precedes_batch_only_and_rejects_unexpected_argv() {
    let (root, sources) = fixture();
    let wave = PlannedWave::create(root.path(), &sources).unwrap();
    let job = &wave.jobs()[0];
    let mut args = vec![
        "--batch".into(),
        "-f".into(),
        "byte-compile-refresh-preloaded".into(),
        "-f".into(),
        "batch-byte-compile".into(),
        job.source.as_os_str().to_os_string(),
    ];
    job.redirect_args(&mut args).unwrap();
    assert_eq!(args[2], "byte-compile-refresh-preloaded");
    assert_eq!(args[3], "--eval");
    let guard = args[4].to_str().unwrap();
    assert!(guard.contains("Unexpected nested byte-compile target"));
    assert!(guard.contains("(expand-file-name file)"));
    assert!(guard.contains(job.pending.to_str().unwrap()));
    assert!(job.redirect_args(&mut vec![]).is_err());
    wave.retain();
}

#[cfg(unix)]
#[test]
fn publication_renames_exact_inode_permissions_and_mtime() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (root, sources) = fixture();
    let wave = PlannedWave::create(root.path(), &sources).unwrap();
    let mut pending = Vec::new();
    for job in wave.jobs() {
        fs::write(&job.pending, b"exact bytecode").unwrap();
        fs::set_permissions(&job.pending, fs::Permissions::from_mode(0o640)).unwrap();
        pending.push(fs::metadata(&job.pending).unwrap());
    }
    wave.validate()
        .unwrap()
        .publish()
        .unwrap()
        .finish()
        .unwrap();
    for (source, before) in sources.iter().zip(pending) {
        let after = fs::metadata(source.with_extension("elc")).unwrap();
        assert_eq!(
            (after.ino(), after.mode(), after.mtime(), after.mtime_nsec()),
            (
                before.ino(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec()
            )
        );
    }
}

#[test]
fn wave_rejects_two_distinct_source_spellings_for_one_destination() {
    let (root, sources) = fixture();
    let alias = root.path().join("sub/../a.el");
    let error = PlannedWave::create(root.path(), &[sources[0].clone(), alias])
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("duplicate bytecode wave destination")
    );
}

#[test]
fn same_basename_in_distinct_directories_has_independent_pending_outputs() {
    let (root, _) = fixture();
    let sources = vec![root.path().join("a.el"), root.path().join("sub/a.el")];
    fs::write(&sources[1], b"different source").unwrap();
    let wave = PlannedWave::create(root.path(), &sources).unwrap();
    assert_ne!(wave.jobs()[0].pending, wave.jobs()[1].pending);
    fs::write(&wave.jobs()[0].pending, b"first product").unwrap();
    fs::write(&wave.jobs()[1].pending, b"second product").unwrap();
    wave.validate()
        .unwrap()
        .publish()
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        fs::read(sources[0].with_extension("elc")).unwrap(),
        b"first product"
    );
    assert_eq!(
        fs::read(sources[1].with_extension("elc")).unwrap(),
        b"second product"
    );
}

#[cfg(unix)]
#[test]
fn source_symlink_is_rejected_before_workers() {
    use std::os::unix::fs::symlink;
    let (root, sources) = fixture();
    let link = root.path().join("alias.el");
    symlink(&sources[0], &link).unwrap();
    let error = PlannedWave::create(root.path(), &[link]).err().unwrap();
    assert!(error.to_string().contains("regular, not a symlink"));
}

#[cfg(unix)]
#[test]
fn pending_symlink_cannot_pass_regular_output_validation() {
    use std::os::unix::fs::symlink;
    let (root, sources) = fixture();
    let wave = PlannedWave::create(root.path(), &sources[..1]).unwrap();
    let staging = wave.directory.path().to_path_buf();
    symlink(&sources[0], &wave.jobs()[0].pending).unwrap();
    assert!(wave.validate().is_err());
    assert!(staging.is_dir());
    assert!(!sources[0].with_extension("elc").exists());
}

#[test]
fn empty_and_directory_pending_outputs_cannot_be_published() {
    let (root, sources) = fixture();
    let empty = PlannedWave::create(root.path(), &sources[..1]).unwrap();
    fs::write(&empty.jobs()[0].pending, b"").unwrap();
    assert!(empty.validate().is_err());
    let directory = PlannedWave::create(root.path(), &sources[..1]).unwrap();
    fs::create_dir(&directory.jobs()[0].pending).unwrap();
    assert!(directory.validate().is_err());
    assert!(!sources[0].with_extension("elc").exists());
}

#[test]
fn publication_failure_reports_partial_products_and_retains_unpublished_output() {
    let (root, sources) = fixture();
    let wave = PlannedWave::create(root.path(), &sources).unwrap();
    let staging = wave.directory.path().to_path_buf();
    for job in wave.jobs() {
        fs::write(&job.pending, b"complete product").unwrap();
    }
    let second_pending = wave.jobs()[1].pending.clone();
    // Destination directory forces the second rename to fail after the first.
    fs::create_dir(sources[1].with_extension("elc")).unwrap();
    let error = wave.validate().unwrap().publish().err().unwrap();
    assert!(
        error
            .to_string()
            .contains(sources[0].with_extension("elc").to_str().unwrap())
    );
    assert!(error.to_string().contains(staging.to_str().unwrap()));
    assert_eq!(
        fs::read(sources[0].with_extension("elc")).unwrap(),
        b"complete product"
    );
    assert_eq!(fs::read(second_pending).unwrap(), b"complete product");
    assert!(sources[1].with_extension("elc").is_dir());
}
