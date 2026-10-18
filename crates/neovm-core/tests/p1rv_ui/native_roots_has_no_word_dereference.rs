use neovm_core::tagged::gc::NativeRoots;

fn probe(roots: &NativeRoots<'_>) {
    let _value = roots.word_value(0);
}

fn main() {}
