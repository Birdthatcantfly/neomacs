use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess};
use neovm_core::tagged::transport::SharedRoot;
use neovm_core::tagged::value::TaggedValue;

fn raw_input(
    run: &mut RunAccess<'_, '_>,
    car: &TaggedValue,
    cdr: &SharedRoot,
    result: &mut OwnerRootSlot<'_>,
) {
    drop(run.cons_into(car, cdr, result));
}

fn main() {}
