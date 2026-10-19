use neovm_core::tagged::gc::Mutator;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadSharing;

fn main() {
    ThreadSharing::<Mutator<'static>>::check();
}
