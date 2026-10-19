use neovm_core::tagged::mutate::CharTableWrite;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn main() {
    ThreadTransfer::<CharTableWrite<'static>>::check();
}
