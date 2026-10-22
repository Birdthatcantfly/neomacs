use super::NumericProgram;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use crate::Error;

#[cfg(test)]
#[path = "numeric_corpus.rs"]
mod corpus;
#[cfg(test)]
#[path = "numeric_reference.rs"]
mod reference;

fn params(left: i32, right: i32) -> [i32; 9] {
    [left, right, 34, 51, 0, -1, i32::MIN, i32::MAX, 9]
}

fn generated() -> Vec<Vec<u8>> {
    let mut formats: Vec<Vec<u8>> = corpus::EXISTING.iter().map(|s| s.to_vec()).collect();
    // Every possible byte after percent, plus every complete parameter and
    // variable directive: malformed/unsupported ones must agree on rejection.
    for byte in u8::MIN..=u8::MAX {
        formats.push(vec![b'%', byte]);
    }
    for byte in b'0'..=b':' {
        formats.push(vec![b'%', b'p', byte, b'%', b'd']);
    }
    for name in (b'a'..=b'z').chain(b'A'..=b'Z') {
        formats.push(vec![
            b'%', b'p', b'1', b'%', b'P', name, b'%', b'g', name, b'%', b'd',
        ]);
        formats.push(vec![b'%', b'g', name, b'%', b'd']);
        formats.push(vec![
            b'%', b'p', b'1', b'%', b'g', name, b'%', b'/', b'%', b'd',
        ]);
    }
    for byte in u8::MIN..=u8::MAX {
        formats.push(vec![b'%', b'\'', byte, b'\'', b'%', b'd']);
    }
    for constant in [
        "",
        "0",
        "1",
        "0000000000",
        "2147483646",
        "2147483647",
        "2147483648",
        "4294967295",
        "9999999999",
        "10000000000",
        "-1",
        "-2147483648",
    ] {
        formats.push(format!("%{{{constant}}}%d").into_bytes());
    }
    for flags in ["", " ", "#", ":", ":-", ": #", ":- #"] {
        for width in ["", "0", "1", "04", "9999", "10000", "10001"] {
            for precision in ["", ".0", ".2", ".9999", ".10000", ".10001"] {
                for conversion in ["d", "o", "x", "X", "c", "s", "l"] {
                    formats.push(format!("%p1%{flags}{width}{precision}{conversion}").into_bytes());
                }
            }
        }
    }
    for tail in [
        b"%".as_slice(),
        b"%p",
        b"%P",
        b"%g",
        b"%{",
        b"%'",
        b"%:",
        b"%:10.",
        b"%p0",
        b"%p:",
        b"%\0",
        b"a\0b",
        b"%p1%:10.2s",
    ] {
        formats.push(tail.to_vec());
        let mut prefixed = b"literal%%".to_vec();
        prefixed.extend_from_slice(tail);
        formats.push(prefixed);
    }
    for op in ["%/", "%m"] {
        for template in [
            "%p1%p2{op}%d",
            "%p1%gA{op}%d",
            "%p1%ga{op}%d",
            "%p1%?%p2%t%{2}%e%{0}%{1}%-%;{op}%d",
            "%p1%{2147483647}%{2}%*%{1}%+{op}%d",
            "%i%i%p1%p2{op}%d",
            "%p1%p2%{4}{op}%d",
        ] {
            formats.push(template.replace("{op}", op).into_bytes());
        }
        formats.push(format!("{}%p1%p2%{{4}}{op}%d", "%{0}".repeat(18)).into_bytes());
        formats.push(format!("{}%{{256}}{op}%d", "%p1".repeat(20)).into_bytes());
    }
    formats.push(format!("{}%p1%{{2}}%/%d", "%p1%d".repeat(20)).into_bytes());
    formats.push(format!("{}%{{2}}%/%d", "%p1".repeat(20)).into_bytes());
    formats.sort();
    formats.dedup();
    formats
}

fn reset_variables() {
    // Reset both cases using the old validator. Lowercase native lifetime is
    // intentionally not assumed; the same native version/state seeds each arm.
    let mut reset = Vec::new();
    for name in (b'a'..=b'z').chain(b'A'..=b'Z') {
        reset.extend_from_slice(b"%{0}%P");
        reset.push(name);
    }
    reference::expand(&reset, [0; 9]).unwrap();
}

fn state_bytes() -> Vec<u8> {
    let mut read = Vec::new();
    for name in (b'a'..=b'z').chain(b'A'..=b'Z') {
        read.extend_from_slice(&[b'%', b'g', name, b'%', b'd', b';']);
    }
    reference::expand(&read, [0; 9]).unwrap()
}

fn compare_expansion(sequence: &[u8], parameters: [i32; 9]) {
    let old_valid = reference::validate(sequence).map(|_| ());
    let program = NumericProgram::new(sequence);
    assert_eq!(
        old_valid,
        program.as_ref().map(|_| ()).map_err(|e| *e),
        "validation {sequence:?}"
    );
    let Ok(program) = program else {
        return;
    };
    reset_variables();
    let old = reference::expand(sequence, parameters);
    let old_state = state_bytes();
    reset_variables();
    let prepared = program.expand(parameters);
    let new_state = state_bytes();
    assert_eq!(
        old, prepared,
        "output {sequence:?}, parameters {parameters:?}"
    );
    assert_eq!(old_state, new_state, "native state effects {sequence:?}");
}

#[test]
fn numeric_constructor_matches_frozen_validator_corpus() {
    let formats = generated();
    assert!(formats.len() > 2000);
    for format in formats {
        assert_eq!(
            reference::validate(&format).map(|_| ()),
            NumericProgram::new(&format).map(|_| ()),
            "{format:?}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn numeric_prepared_expansion_matches_reference_corpus_and_directives() {
    for format in generated() {
        compare_expansion(&format, params(7, 2));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn numeric_prepared_division_matches_reference_edges() {
    for format in generated()
        .into_iter()
        .filter(|s| s.windows(2).any(|p| p == b"%/" || p == b"%m"))
    {
        for (left, right) in [
            (7, 0),
            (7, -1),
            (i32::MIN, 0),
            (i32::MIN, -1),
            (i32::MIN, 2),
            (i32::MAX, -2),
        ] {
            compare_expansion(&format, params(left, right));
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn numeric_constructor_does_not_execute_native_assignments() {
    reference::expand(b"%{77}%PA", [0; 9]).unwrap();
    let _program = NumericProgram::new(b"%{1}%PA%p1%gA%/%d").unwrap();
    assert_eq!(reference::expand(b"%gA%d", [0; 9]).unwrap(), b"77");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn numeric_prepared_program_reuses_owned_source() {
    let mut source = b"%p1%d;%p2%d".to_vec();
    let program = NumericProgram::new(&source).unwrap();
    source.fill(b'x');
    assert_eq!(program.as_bytes(), b"%p1%d;%p2%d");
    for number in [i32::MIN, -1, 0, 7, i32::MAX] {
        assert_eq!(
            program.expand(params(number, 2)).unwrap(),
            format!("{number};2").as_bytes()
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn numeric_prepared_program_concurrent_expansion() {
    let program = NumericProgram::new(b"%p1%PA%p1%gA%/%d").unwrap();
    std::thread::scope(|scope| {
        for number in 1..=8 {
            let program = &program;
            scope.spawn(move || {
                for _ in 0..200 {
                    assert_eq!(program.expand(params(number, 2)).unwrap(), b"1");
                }
            });
        }
    });
}

#[test]
fn numeric_program_thread_contract() {
    static_assertions::assert_impl_all!(NumericProgram: Send, Sync);
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn numeric_platform_error_order_is_unchanged() {
    assert_eq!(
        super::expand_numeric(b"%s", [0; 9]),
        Err(Error::InvalidNumericFormat)
    );
    assert_eq!(
        super::expand_numeric(b"%p1%d", [0; 9]),
        Err(Error::UnsupportedPlatform)
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn numeric_prepared_persistent_variable_trace_matches_reference() {
    let trace: &[(&[u8], [i32; 9])] = &[
        (b"%{10}%PA", [0; 9]),
        (b"%gA%d;%gA%{1}%+%PA", [0; 9]),
        (b"%gA%d", [0; 9]),
        (b"%p1%gA%/%d", params(i32::MIN, -1)),
        (b"%{0}%{1}%-%PA", [0; 9]),
        (b"%p1%gA%/%d", params(i32::MIN, -1)),
        (b"%gA%d", [0; 9]),
        (b"%{7}%Pa%ga%d", [0; 9]),
        (b"%ga%d", [0; 9]),
    ];
    // Compile the entire trace before either native execution. Construction
    // cannot take a variable snapshot that would hide the later changes.
    let programs: Vec<_> = trace
        .iter()
        .map(|(s, _)| NumericProgram::new(s).unwrap())
        .collect();
    reset_variables();
    let old: Vec<_> = trace
        .iter()
        .map(|(s, p)| reference::expand(s, *p))
        .collect();
    let old_state = state_bytes();
    reset_variables();
    let new: Vec<_> = programs
        .iter()
        .zip(trace)
        .map(|(program, (_, p))| program.expand(*p))
        .collect();
    let new_state = state_bytes();
    assert_eq!(old, new);
    assert_eq!(old_state, new_state);
}
