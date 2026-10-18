//! The authority-word methods exist only on the sealed lease types; a
//! raw root-slot word has no reach into the protocol.

use neovm_core::tagged::gc::OwnerRootSlot;

fn probe(home: &OwnerRootSlot<'_>) {
    home.authority_word();
}

fn main() {}
