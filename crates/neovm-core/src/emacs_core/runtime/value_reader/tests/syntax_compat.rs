use super::*;

fn read_result(input: &str) -> Result<Option<(Value, usize)>, ReadError> {
    let ob = crate::emacs_core::symbol::Obarray::new();
    read_one(input, 0, &ob)
}

#[test]
fn rea09_property_triples_replace_and_validate_like_set_text_properties() {
    let value = read1(r#"#("abc" 0 1 (a b) 0 2 (c d))"#);
    let runs = get_string_text_properties_for_value(value).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].start, runs[0].end), (0, 2));
    assert!(runs[0].plist.cons_car().is_symbol_named("c"));
    let value = read1(r#"#("abc" 2 1 face)"#);
    let runs = get_string_text_properties_for_value(value).unwrap();
    assert_eq!((runs[0].start, runs[0].end), (1, 2));
    assert!(runs[0].plist.cons_car().is_symbol_named("face"));
    let error = read_result(r#"#("abc" 0 1 (a))"#).unwrap_err();
    assert_eq!(error.signal_symbol.as_deref(), Some("error"));
    assert_eq!(error.message, "Odd length text property list");
    let error = read_result(r#"#("abc" a 1 (a))"#).unwrap_err();
    assert_eq!(error.message, "Odd length text property list");
    let error = read_result(r#"#("abc" a 1 (a b))"#).unwrap_err();
    assert_eq!(error.signal_symbol.as_deref(), Some("wrong-type-argument"));
    assert!(error.signal_data[0].is_symbol_named("integer-or-marker-p"));
    assert_eq!(read_result(r#"#("a" . nil)"#).unwrap_err().message, ".");
}
