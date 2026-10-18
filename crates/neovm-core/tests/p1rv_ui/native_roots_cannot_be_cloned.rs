use neovm_core::tagged::gc::NativeRoots;

fn duplicate(roots: NativeRoots<'static>) {
    let copy = roots.clone();
    let _ = (roots, copy);
}

fn main() {}
