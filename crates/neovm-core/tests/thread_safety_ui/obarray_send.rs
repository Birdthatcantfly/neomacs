use neovm_core::emacs_core::Obarray;

#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

// The owner stays on its mutator; only its admitted scan snapshot moves.
fn main() {
    ThreadTransfer::<Obarray>::check();
}
