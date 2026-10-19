//! The keeper lease borrows the guard, not merely its era lifetime.
use neovm_core::tagged::gc::{AllStopped, KeeperTableMut};
use neovm_core::tagged::value::TaggedValue;

fn main() {
    fn escape<'stop>(
        stopped: &'stop AllStopped<'static>,
        table: TaggedValue,
    ) -> KeeperTableMut<'static> {
        KeeperTableMut::enter(stopped, table).unwrap()
    }
    let _ = escape;
}
