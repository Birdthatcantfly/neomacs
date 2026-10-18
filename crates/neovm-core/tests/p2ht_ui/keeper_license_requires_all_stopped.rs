//! The keeper license is minted from AllStopped, never from a running
//! owner: a Mutator cannot hand out stop-all editing rights.

use neovm_core::tagged::gc::{KeeperTableMut, Mutator};
use neovm_core::tagged::value::TaggedValue;

fn main() {
    fn mint(owner: &mut Mutator<'static>, table: TaggedValue) {
        let _license = KeeperTableMut::enter(owner, table);
    }
    let _ = mint;
}
