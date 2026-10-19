#[path = "scan_contract_shim.rs"]
mod tagged;
#[path = "thread_bounds.rs"]
mod thread_bounds;

use tagged::gc::{TaggedHeap, scan_contract};
use thread_bounds::{ThreadSharing, ThreadTransfer};

// Stub fields must not supply an accidental confinement failure.
static_assertions::assert_impl_all!(TaggedHeap: Send, Sync);
static_assertions::assert_impl_all!(scan_contract::ScanStorageOwner: Send, Sync);

fn main() {
    ThreadTransfer::<scan_contract::ScanStorageLease>::check();
    ThreadTransfer::<u32>::transfer(1);
    ThreadSharing::<u32>::check();
    let mut heap = TaggedHeap::default();
    // SAFETY: this fixture owns the heap and has no TLS aliases or other writers.
    let world = unsafe { scan_contract::SingleMutatorWorld::from_heap(&mut heap) };
    let mut owner = scan_contract::ScanStorageOwner::new();
    let lease = scan_contract::ScanStorageLease::capture(&owner, &world);
    assert!(owner.has_leases());
    drop(lease);
    assert!(!owner.has_leases());
    assert_eq!(world.heap_identity(), 1);
}
