use neovm_core::emacs_core::intern::SymId;
use neovm_core::emacs_core::symbol::{CanonicalAccess, Obarray};
use neovm_core::emacs_core::value::Value;

// Canonical readers return copied words, never references to mutable slots.
fn borrowed_global(owner: &Obarray, access: CanonicalAccess) -> Option<&Value> {
    owner.canonical_value_by_name(access, "p74-no-slot-borrow")
}

fn borrowed_identity(owner: &Obarray, access: CanonicalAccess, symbol: SymId) -> Option<&Value> {
    owner.canonical_value(access, symbol)
}

fn borrowed_default(owner: &Obarray, access: CanonicalAccess, symbol: SymId) -> Option<&Value> {
    owner.canonical_default_value(access, symbol)
}

fn main() {}
