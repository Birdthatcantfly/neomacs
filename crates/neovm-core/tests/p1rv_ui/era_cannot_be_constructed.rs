use neovm_core::tagged::gc::ParallelHeapEra;

fn fabricate() -> ParallelHeapEra<'static> {
    ParallelHeapEra {}
}

fn main() {}
