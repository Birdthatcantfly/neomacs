use neovm_core::tagged::value::TaggedValue;

// A background payload carries plain data or rooted transport, never a raw
// thread-confined value.
struct WorkerPayload {
    _value: TaggedValue,
}

#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn main() {
    ThreadTransfer::<WorkerPayload>::check();
}
