use neovm_core::tagged::gc::{Mutator, NativeRoots};
use neovm_core::tagged::transport::SharedRoot;

fn leak(mutator: &mut Mutator<'_>, roots: &[SharedRoot]) {
    let _ = mutator.with_native_extent(roots, |handles: &NativeRoots<'_>| handles);
}

fn main() {}
