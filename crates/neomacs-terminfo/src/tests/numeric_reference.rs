//! Frozen independent pre-A2 TOKEN validator. Never call NumericProgram::new.
//! Original numeric.rs SHA256: 596fb1d77cd0dd6c8722d63abd30c977ff15c2f6c7d953a844da15dff22a734e
//! Numeric argument validation; ncurses owns formatting and variable state.
use crate::Error;
use regex::bytes::Regex;
use std::sync::LazyLock;

// Recognize complete directives, including printf flags, before calling C.
// In particular %s, %:10.2s and %l must never make numeric slots into pointers.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x-u)(?:
    [^%\x00]+ | %% | %[i+*/m&|^=><AO!~?te;-]
    | %p[1-9] | %[Pg][a-zA-Z] | %'[^\x00]'
    | %\{(?P<constant>[0-9]{1,10})\}
    | %(?::[-\x20\#]*)?[\x20\#]*(?:[0-9]{0,4}|10000)
        (?:\.(?:[0-9]{1,4}|10000))?[doxXc]
)",
    )
    .expect("fixed numeric terminfo grammar")
});

use std::ops::Range;

pub(super) fn validate(sequence: &[u8]) -> Result<Vec<Range<usize>>, Error> {
    let mut end = 0;
    let mut tokens = Vec::new();
    for token in TOKEN.captures_iter(sequence) {
        let matched = token.get(0).expect("a capture has a full match");
        if matched.start() != end {
            return Err(Error::InvalidNumericFormat);
        }
        end = matched.end();
        if let Some(constant) = token.name("constant")
            && std::str::from_utf8(constant.as_bytes())
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .is_none()
        {
            return Err(Error::InvalidNumericFormat);
        }
        tokens.push(matched.range());
    }
    if end != sequence.len() {
        return Err(Error::InvalidNumericFormat);
    }
    Ok(tokens)
}

// The grammar reference is independent. Native formatting and the unchanged
// arithmetic contract still use the same audited native serialization seam.
pub(super) fn expand(sequence: &[u8], parameters: [i32; 9]) -> Result<Vec<u8>, Error> {
    super::super::NumericProgram::from_validated(sequence, validate(sequence)?)?.expand(parameters)
}
