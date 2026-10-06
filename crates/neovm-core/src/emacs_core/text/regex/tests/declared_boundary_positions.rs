//! Per-opcode callback order/cache identity: public GNU parity is tested separately.
use super::*;
use crate::emacs_core::syntax::{
    StringSyntaxPositionError, StringSyntaxPositionSource, SyntaxProperties, SyntaxTable,
};
use crate::heap_types::{GnuDeclaredCharacterCount, GnuStringCharPos, LispString};
use std::cell::RefCell;

fn request(kind: SyntaxAssertion, before: char, after: char) -> BoundarySyntaxRequest {
    BoundarySyntaxRequest {
        kind,
        current_byte: crate::buffer::EmacsBytePos::new(3),
        previous: Some(BoundarySyntaxCharacter {
            character: crate::emacs_core::emacs_char::EmacsChar::from_code_unchecked(before as u32),
            byte_position: crate::buffer::EmacsBytePos::new(2),
        }),
        current: Some(BoundarySyntaxCharacter {
            character: crate::emacs_core::emacs_char::EmacsChar::from_code_unchecked(after as u32),
            byte_position: crate::buffer::EmacsBytePos::new(3),
        }),
    }
}

#[derive(Default)]
struct PhysicalReads(RefCell<Vec<usize>>);
impl SyntaxLookup for PhysicalReads {
    fn char_syntax(&self, c: char) -> SyntaxClass {
        if c == '!' {
            SyntaxClass::Punctuation
        } else {
            SyntaxClass::Word
        }
    }
    fn char_syntax_at(&self, c: char, pos: usize) -> SyntaxClass {
        self.0.borrow_mut().push(pos);
        self.char_syntax(c)
    }
    fn char_has_category(&self, _: char, _: u8) -> bool {
        false
    }
    fn cache_key(&self) -> SyntaxCacheKey {
        SyntaxCacheKey::Standard
    }
}

#[test]
fn ordinary_boundary_pair_keeps_physical_lookup_order_and_short_circuit() {
    let lookup = PhysicalReads::default();
    let classes = lookup.boundary_syntax_at(request(SyntaxAssertion::WordBoundary, 'c', 'd'));
    assert_eq!(*lookup.0.borrow(), [2, 3]);
    assert_eq!(
        classes.previous(),
        Some((
            crate::emacs_core::emacs_char::EmacsChar::from_code_unchecked('c' as u32),
            SyntaxClass::Word
        ))
    );
    assert_eq!(
        classes.current(),
        Some((
            crate::emacs_core::emacs_char::EmacsChar::from_code_unchecked('d' as u32),
            SyntaxClass::Word
        ))
    );
    lookup.0.borrow_mut().clear();
    lookup.boundary_syntax_at(request(SyntaxAssertion::WordBeginning, 'c', '!'));
    assert_eq!(*lookup.0.borrow(), [3]);
    lookup.0.borrow_mut().clear();
    lookup.boundary_syntax_at(request(SyntaxAssertion::WordEnd, '!', 'd'));
    assert_eq!(*lookup.0.borrow(), [2]);
}

fn supplied(bytes: &[u8]) -> Value {
    Value::heap_string(LispString::from_emacs_bytes_with_declared_chars(
        bytes.to_vec(),
        GnuDeclaredCharacterCount::try_from(2).expect("bounded test metadata"),
    ))
}

#[test]
fn supplied_boundary_converts_current_once_before_previous_logical_lookup() {
    let context = crate::emacs_core::eval::Context::new();
    let source = supplied(b"abcde");
    let base = BufferSyntaxLookup {
        syntax_table: SyntaxTable::for_buffer(
            context.buffers.current_buffer().expect("test buffer"),
        ),
        category_table: None,
        word_boundary: WordBoundaryLookup::default(),
    };
    let lookup = crate::emacs_core::regex::StringSyntaxLookup::for_source(
        Some(base),
        StringSyntaxPositionSource::try_from(&source).expect("validated test source"),
        SyntaxProperties::for_scan(true, &context.obarray, &context.buffers),
    );
    crate::emacs_core::string_pos_cache::reset_string_pos_cache();
    lookup
        .as_lookup()
        .boundary_syntax_at(request(SyntaxAssertion::WordBoundary, 'c', 'd'));
    assert_eq!(
        lookup.position_error(),
        Some(StringSyntaxPositionError::OutOfRange {
            position: GnuStringCharPos::new(-1)
        })
    );
    // A logical previous read leaves the actual byte3/char0 cache anchor alone.
    assert_eq!(
        crate::emacs_core::string_pos_cache::string_byte_to_gnu_char(
            source,
            source.as_lisp_string().expect("test payload"),
            2
        )
        .get(),
        -1
    );
    crate::emacs_core::string_pos_cache::reset_string_pos_cache();
}

#[test]
fn supplied_symbol_beginning_skips_invalid_previous_after_nonword_current() {
    let context = crate::emacs_core::eval::Context::new();
    let source = supplied(b"abc!e");
    let base = BufferSyntaxLookup {
        syntax_table: SyntaxTable::for_buffer(
            context.buffers.current_buffer().expect("test buffer"),
        ),
        category_table: None,
        word_boundary: WordBoundaryLookup::default(),
    };
    let lookup = crate::emacs_core::regex::StringSyntaxLookup::for_source(
        Some(base),
        StringSyntaxPositionSource::try_from(&source).expect("validated test source"),
        SyntaxProperties::for_scan(true, &context.obarray, &context.buffers),
    );
    crate::emacs_core::string_pos_cache::reset_string_pos_cache();
    let classes =
        lookup
            .as_lookup()
            .boundary_syntax_at(request(SyntaxAssertion::SymbolBeginning, 'c', '!'));
    assert_eq!(classes.previous(), None);
    assert_eq!(
        classes.current(),
        Some((
            crate::emacs_core::emacs_char::EmacsChar::from_code_unchecked('!' as u32),
            SyntaxClass::Punctuation
        ))
    );
    assert_eq!(lookup.position_error(), None);
    crate::emacs_core::string_pos_cache::reset_string_pos_cache();
}
