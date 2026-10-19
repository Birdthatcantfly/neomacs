use neovm_core::tagged::transport::LocalRoot;

#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn main() {
    ThreadTransfer::<LocalRoot<'static>>::check();
}
