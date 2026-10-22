//! Child test module of engine::window_input_ownership; pure explicit policies.
use super::*;

#[test]
fn ownership_policy_defaults_in_place_and_accepts_explicit_on() {
    assert_eq!(
        WindowInputOwnership::from_setting(None),
        WindowInputOwnership::InPlace
    );
    for setting in ["on", "1", "true", "yes", " ON ", "TrUe"] {
        assert_eq!(
            WindowInputOwnership::from_setting(Some(OsStr::new(setting))),
            WindowInputOwnership::InPlace,
            "{setting}"
        );
    }
}

#[test]
fn ownership_policy_explicit_invalid_and_off_values_keep_legacy() {
    for setting in ["off", "0", "false", "no", "", " ", "auto", "on-no"] {
        assert_eq!(
            WindowInputOwnership::from_setting(Some(OsStr::new(setting))),
            WindowInputOwnership::LegacyCopies,
            "{setting}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            WindowInputOwnership::from_setting(Some(OsStr::from_bytes(b"\xffon"))),
            WindowInputOwnership::LegacyCopies
        );
    }
}
