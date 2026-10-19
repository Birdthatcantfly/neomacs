use neovm_core::tagged::gc::ParallelHeapEra;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(era: ParallelHeapEra<'static>) {
    ThreadTransfer::<ParallelHeapEra<'static>>::transfer(era);
}

fn main() {}
