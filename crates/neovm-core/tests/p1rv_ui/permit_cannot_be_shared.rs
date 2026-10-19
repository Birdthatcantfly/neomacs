use neovm_core::tagged::gc::MutatorPermit;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

// A scoped worker can receive &MutatorPermit only if that reference is Send
// (equivalently, MutatorPermit is Sync); no static lifetime is required.
fn share(_: &MutatorPermit<'_>) {
    ThreadTransfer::<&MutatorPermit<'_>>::check();
}

fn main() {}
