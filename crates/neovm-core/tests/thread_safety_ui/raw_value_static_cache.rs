use neovm_core::tagged::value::TaggedValue;
use std::sync::OnceLock;

#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadSharing;

// Shared statics require Sync: test exactly the bound on a process-wide cache.
fn main() {
    ThreadSharing::<OnceLock<TaggedValue>>::check();
}
