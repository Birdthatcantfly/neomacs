use neovm_core::tagged::gc::AllStopped;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(stopped: AllStopped<'static>) {
    ThreadTransfer::<AllStopped<'static>>::transfer(stopped);
}

fn main() {}
