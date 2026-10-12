use neovm_core::emacs_core::forward::CanonicalArm;

// A canonical localized arm names a record of an obarray's BLV pool; only
// the crate can make one.
fn forge(blv: neovm_core::emacs_core::symbol::BlvPtr) -> CanonicalArm {
    CanonicalArm::Localized(neovm_core::emacs_core::forward::CanonicalBlv::new(blv))
}

fn main() {
    let _ = forge;
}
