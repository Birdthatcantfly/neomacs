//! A plain table value has no minting power: entering a lease needs a
//! live run access as the receiver, and the value is only the argument.

use neovm_core::tagged::gc::TableRef;
use neovm_core::tagged::value::TaggedValue;

fn main() {
    let mut table = TaggedValue::fixnum(1);
    let _lease = TableRef::enter(&mut table, table);
}
