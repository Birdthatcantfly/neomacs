#[path = "support/scan_contract_shim.rs"]
mod tagged;
use tagged::gc::{TaggedHeap, scan_contract};
fn main() {
    let heap = TaggedHeap::default();
    // SAFETY: the fixture isolates the constructor's exclusive-borrow contract.
    let _world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&heap) };
}
