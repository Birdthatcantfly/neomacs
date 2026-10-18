use neovm_core::tagged::gc::NativeRoots;

fn probe(roots: &NativeRoots<'_>) {
    let _heap = roots.tagged_heap();
}

fn main() {}
