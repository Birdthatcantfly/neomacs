use crate::*;

#[test]
#[cfg(unix)]
fn explicit_aot_dry_run_executes_the_standard_dump_recipe() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = repository_root().join("tmp");
    fs::create_dir_all(&scratch).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("xtask-dump-recipe-")
        .tempdir_in(scratch)
        .unwrap();
    let mut options = FreshBuildOptions::parse(
        repository_root(),
        ["--profile", "profiling", "--aot-preload", "--dry-run"].map(OsString::from),
    )
    .unwrap();
    options.bin_dir = dir.path().to_path_buf();
    let paths = pipeline_paths(&options);
    let captured = dir.path().join("actual-argv.txt");
    fs::write(
        &paths.temacs,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$XTASK_DUMP_RECIPE_ARGV\"\n",
    )
    .unwrap();
    fs::set_permissions(&paths.temacs, fs::Permissions::from_mode(0o755)).unwrap();

    run_aot_preload_dry_run_gate(
        &options,
        &paths,
        &[(
            OsString::from("XTASK_DUMP_RECIPE_ARGV"),
            captured.as_os_str().to_os_string(),
        )],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(captured).unwrap(),
        "--batch\n--no-build-details\n-l\nloadup\n--temacs=pdump\n"
    );
}
