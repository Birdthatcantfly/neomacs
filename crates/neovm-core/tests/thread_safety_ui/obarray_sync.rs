use neovm_core::emacs_core::Obarray;

#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadSharing;

// Mutable BLVs and local Values are not made shareable by atomic cells.
fn main() {
    ThreadSharing::<Obarray>::check();
}
