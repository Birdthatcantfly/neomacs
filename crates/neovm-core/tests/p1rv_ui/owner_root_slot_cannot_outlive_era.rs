use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess};
use neovm_core::tagged::transport::SharedRoot;

fn escape<'owner, 'era>(
    run: &mut RunAccess<'owner, 'era>,
    source: &SharedRoot,
) -> OwnerRootSlot<'static> {
    run.root_slot(source).unwrap()
}

fn main() {}
