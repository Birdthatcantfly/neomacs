//! Executor op bodies receive branded slice homes, not Lisp values: no
//! method on the receiver materializes a Value or TaggedValue, which is
//! what keeps as_hash_table/with_hash_table_mut unreachable there.

use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess};
use neovm_core::tagged::value::TaggedValue;

fn materialize(run: &mut RunAccess<'_, '_>, home: &OwnerRootSlot<'_>) -> TaggedValue {
    run.with_slice(|slice| slice.bind(home).unwrap().value()).unwrap()
}

fn main() {}
