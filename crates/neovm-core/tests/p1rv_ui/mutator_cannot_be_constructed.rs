use neovm_core::tagged::gc::Mutator;

fn fabricate() -> Mutator<'static> {
    Mutator {}
}

fn main() {}
