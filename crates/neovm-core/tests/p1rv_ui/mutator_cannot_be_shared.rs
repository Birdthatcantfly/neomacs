use neovm_core::tagged::gc::Mutator;

fn require_sync<T: Sync>() {}

fn main() {
    require_sync::<Mutator<'static>>();
}
