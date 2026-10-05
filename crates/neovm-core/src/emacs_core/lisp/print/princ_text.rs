//! Encoding decisions shared by every no-escape string printer.

use crate::emacs_core::emacs_char;
use crate::heap_types::LispString;
use std::borrow::Cow;

/// Whether GNU's stream preparation enabled escaping of raw byte8 characters.
/// This is per-render state, borrowed by one mutator; there is no shared cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrincStringOutput {
    PreserveByte8,
    OctalByte8,
    CallbackCharacters,
}

/// The print target determines how unibyte string bytes become characters.
/// Per-render state only; no mutator-local or shared cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrincStringDestination {
    Text,
    Callable,
}

/// An immutable source borrowed only while constructing output, before callbacks.
/// The LispString constructor establishes each variant's encoding invariant.
#[derive(Clone, Copy, Debug)]
enum PrincStringSource<'a> {
    Unibyte(&'a [u8]),
    Multibyte(&'a [u8]),
}

// These contain immutable bytes or scalar encoding policy only. No heap handle
// or mutator state is transferred with them; the source borrow bounds validity.
static_assertions::assert_impl_all!(PrincStringOutput: Send, Sync);
static_assertions::assert_impl_all!(PrincStringDestination: Send, Sync);
static_assertions::assert_impl_all!(PrincStringSource<'static>: Send, Sync);

impl<'a> From<&'a LispString> for PrincStringSource<'a> {
    fn from(string: &'a LispString) -> Self {
        if string.is_multibyte() {
            Self::Multibyte(string.as_bytes())
        } else {
            Self::Unibyte(string.as_bytes())
        }
    }
}

/// Canonical Emacs bytes suitable for a multibyte sink. Unlike a callable
/// stream, an unibyte high byte becomes byte8 when preserved (print.c:479-495).
/// GNU's print_string_1 / string_escape_byte8 escape byte8 in either encoding
/// and preserve ordinary Unicode and non-Unicode characters (print.c:472-479;
/// character.c:773-835).
pub(crate) fn princ_string_bytes(
    string: &LispString,
    escaping: PrincStringOutput,
) -> Cow<'_, [u8]> {
    match (PrincStringSource::from(string), escaping) {
        (PrincStringSource::Unibyte(bytes), PrincStringOutput::PreserveByte8) => {
            if bytes.is_ascii() {
                Cow::Borrowed(bytes)
            } else {
                Cow::Owned(emacs_char::str_to_multibyte(bytes))
            }
        }
        (PrincStringSource::Unibyte(bytes), PrincStringOutput::OctalByte8) => {
            if bytes.is_ascii() {
                Cow::Borrowed(bytes)
            } else {
                Cow::Owned(crate::emacs_core::string_escape::octal_escape_unibyte_eight_bit(bytes))
            }
        }
        (PrincStringSource::Unibyte(bytes), PrincStringOutput::CallbackCharacters) => {
            if bytes.is_ascii() {
                return Cow::Borrowed(bytes);
            }
            let mut out = Vec::with_capacity(bytes.len());
            for &byte in bytes {
                let mut encoded = [0; emacs_char::MAX_MULTIBYTE_LENGTH];
                let len = emacs_char::char_string(u32::from(byte), &mut encoded);
                out.extend_from_slice(&encoded[..len]);
            }
            Cow::Owned(out)
        }
        (PrincStringSource::Multibyte(bytes), PrincStringOutput::CallbackCharacters) => {
            Cow::Borrowed(bytes)
        }
        (PrincStringSource::Multibyte(bytes), PrincStringOutput::PreserveByte8) => {
            Cow::Borrowed(bytes)
        }
        (PrincStringSource::Multibyte(bytes), PrincStringOutput::OctalByte8) => {
            // GNU character.c:786 skips ASCII multibyte strings by comparing
            // cached character/byte lengths. Ordinary Unicode needs no decode.
            if string.schars() == string.sbytes()
                || !bytes
                    .iter()
                    .any(|byte| emacs_char::char_byte8_head_p(*byte))
            {
                return Cow::Borrowed(bytes);
            }
            let mut out = Vec::with_capacity(bytes.len());
            let mut pos = 0;
            while pos < bytes.len() {
                let (code, len) = emacs_char::string_char(&bytes[pos..]);
                if emacs_char::char_byte8_p(code) {
                    let byte = emacs_char::char_to_byte8(code);
                    out.extend_from_slice(&[
                        b'\\',
                        b'0' + (byte >> 6),
                        b'0' + ((byte >> 3) & 7),
                        b'0' + (byte & 7),
                    ]);
                } else {
                    out.extend_from_slice(&bytes[pos..pos + len]);
                }
                pos += len;
            }
            Cow::Owned(out)
        }
    }
}
