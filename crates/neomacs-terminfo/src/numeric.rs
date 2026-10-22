//! Numeric argument validation; ncurses owns formatting and variable state.
use crate::Error;
use regex::bytes::Regex;
use std::ffi::{CStr, CString};
use std::ops::Range;
use std::sync::LazyLock;

pub(crate) mod division;

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

/// An immutable, validated numeric terminfo capability.
///
/// Construction validates every directive and owns its NUL-terminated source;
/// it never executes native code or changes native variables. The program is
/// `Send + Sync`: each expansion snapshots current ncurses variable state and
/// formats under the existing process-wide native lock. Loading another entry
/// still selects native terminal state; a program does not pin that state.
#[derive(Clone, Debug)]
pub struct NumericProgram {
    format: CString,
    tokens: Vec<Range<usize>>,
    division: Option<division::Program>,
}

impl NumericProgram {
    /// Validate and retain a capability without expanding it.
    ///
    /// String-consuming directives, malformed syntax, embedded NULs and
    /// constants outside the accepted i32 domain return InvalidNumericFormat.
    pub fn new(sequence: &[u8]) -> Result<Self, Error> {
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
        Self::from_validated(sequence, tokens)
    }

    // Private: callers already validated the complete grammar. The cfg(test)
    // frozen reference validator uses this seam without invoking Self::new.
    fn from_validated(sequence: &[u8], tokens: Vec<Range<usize>>) -> Result<Self, Error> {
        let division = tokens
            .iter()
            .any(|range| matches!(&sequence[range.clone()], b"%/" | b"%m"))
            .then(|| division::Program::new(sequence, &tokens));
        let format = CString::new(sequence).map_err(|_| Error::InvalidNumericFormat)?;
        Ok(Self {
            format,
            tokens,
            division,
        })
    }

    /// The unchanged capability bytes, without the terminating NUL.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.format.to_bytes()
    }

    /// Expand with nine signed slots using current native variable state.
    ///
    /// Division/remainder safety depends on these parameters and referenced
    /// native variables and is checked on every call under the native lock.
    /// Native signed division overflow, poisoned native state, unavailable
    /// native expansion or an unsupported platform returns the existing Error.
    pub fn expand(&self, parameters: [i32; 9]) -> Result<Vec<u8>, Error> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            crate::native::expand(self, parameters)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = parameters;
            Err(Error::UnsupportedPlatform)
        }
    }

    pub(crate) fn format(&self) -> &CStr {
        &self.format
    }
    pub(crate) fn tokens(&self) -> &[Range<usize>] {
        &self.tokens
    }
    pub(crate) fn division(&self) -> Option<&division::Program> {
        self.division.as_ref()
    }
}

impl AsRef<[u8]> for NumericProgram {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl TryFrom<&[u8]> for NumericProgram {
    type Error = Error;
    fn try_from(sequence: &[u8]) -> Result<Self, Self::Error> {
        Self::new(sequence)
    }
}

/// Validate and expand one numeric terminfo program.
///
/// This preserves the one-shot API. Repeated terminal output should retain a
/// NumericProgram instead of repeating validation and CString construction.
/// Error ordering is unchanged: validate first, then platform/native checks.
pub fn expand_numeric(sequence: &[u8], parameters: [i32; 9]) -> Result<Vec<u8>, Error> {
    NumericProgram::new(sequence)?.expand(parameters)
}

#[cfg(test)]
#[path = "tests/numeric_program_test.rs"]
mod numeric_program_test;
