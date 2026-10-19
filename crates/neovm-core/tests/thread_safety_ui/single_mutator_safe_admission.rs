#[path = "support/scan_contract_shim.rs"]
mod tagged;
use tagged::gc::{TaggedHeap, scan_contract};
fn main() {
    let mut heap = TaggedHeap::default();
    let _world = scan_contract::SingleMutatorWorld::from_heap(&mut heap);
}
