#[path = "support/scan_contract_shim.rs"]
mod tagged;
use tagged::gc::scan_contract;
#[path = "support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadSharing;

// A lease may move to its reader, but cannot be shared among readers.
fn main() {
    ThreadSharing::<scan_contract::ScanStorageLease>::check();
}
