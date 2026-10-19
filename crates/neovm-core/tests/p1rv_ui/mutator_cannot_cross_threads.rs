use neovm_core::tagged::gc::Mutator;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(mutator: Mutator<'static>) {
    ThreadTransfer::<Mutator<'static>>::transfer(mutator);
}

fn main() {}
