use super::*;

#[test]
fn numeric_controls_prepare_once_and_preserve_original_bytes() {
    let mut database = Database([("cm", b"G%p1%d,%p2%d;".as_slice()), ("IC", b"I%p1%d;")].into());
    let caps = Capabilities::from_database(&mut database, "").unwrap();
    assert_eq!(caps.control("cm"), b"G%p1%d,%p2%d;");
    assert!(matches!(
        caps.strings.get("cm"),
        Some(Control::Numeric { program: Ok(_), .. })
    ));
    assert!(matches!(
        caps.strings.get("IC"),
        Some(Control::Numeric { program: Ok(_), .. })
    ));
    for value in [0, 1, 7, 79] {
        let mut parameters = [0; 9];
        parameters[0] = value;
        parameters[1] = value;
        assert_eq!(
            caps.expand_control("cm", parameters).unwrap(),
            neomacs_terminfo::expand_numeric(caps.control("cm"), parameters).unwrap()
        );
        assert_eq!(
            caps.expand_control("IC", parameters).unwrap(),
            neomacs_terminfo::expand_numeric(caps.control("IC"), parameters).unwrap()
        );
    }
}

#[test]
fn invalid_insert_program_stays_present_until_original_use_site() {
    let mut database = Database([("cm", b"G%p1%d,%p2%d;".as_slice()), ("IC", b"%s")].into());
    let caps = Capabilities::from_database(&mut database, "").unwrap();
    assert_eq!(caps.control("IC"), b"%s");
    assert!(matches!(
        caps.strings.get("IC"),
        Some(Control::Numeric {
            program: Err(neomacs_terminfo::Error::InvalidNumericFormat),
            ..
        })
    ));
    assert!(caps.expand_control("IC", [0; 9]).is_err());
    let mut invalid_cm = Database([("cm", b"%s".as_slice())].into());
    assert!(Capabilities::from_database(&mut invalid_cm, "").is_err());
}
