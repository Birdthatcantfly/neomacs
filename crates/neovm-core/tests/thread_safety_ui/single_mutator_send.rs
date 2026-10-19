#[path = "support/scan_contract_shim.rs"]
mod tagged;
use tagged::gc::scan_contract;
#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

// Exclusive scan admission stays on the heap owner thread.
fn main() {
    ThreadTransfer::<scan_contract::SingleMutatorWorld<'static>>::check();
}
