use neovm_core::tagged::gc::ConcurrentMarkCapture;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(capture: ConcurrentMarkCapture<'static>) {
    ThreadTransfer::<ConcurrentMarkCapture<'static>>::transfer(capture);
}

fn main() {}
