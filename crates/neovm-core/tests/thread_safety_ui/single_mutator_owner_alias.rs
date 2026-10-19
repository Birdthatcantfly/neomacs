#[path = "support/scan_contract_shim.rs"]
mod tagged;
use tagged::gc::{TaggedHeap, scan_contract};
fn main() {
    let mut heap = TaggedHeap::default();
    // SAFETY: the fixture isolates the lifetime of exclusive capture admission.
    let world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&mut heap) };
    let _identity = heap.identity();
    let _captured_identity = world.heap_identity();
}
