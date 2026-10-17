use neovm_core::tagged::gc::ParallelHeapEra;

fn require_sync<T: Sync>() {}

fn main() {
    require_sync::<ParallelHeapEra<'static>>();
}
