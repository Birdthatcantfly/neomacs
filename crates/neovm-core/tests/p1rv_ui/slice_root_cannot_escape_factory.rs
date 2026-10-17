use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess, SliceRoot};

fn escape(run: &mut RunAccess<'_, '_>, home: &OwnerRootSlot<'_>) -> SliceRoot<'static> {
    run.with_slice(|slice| slice.bind(home).unwrap()).unwrap()
}

fn main() {}
