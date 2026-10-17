use neovm_core::tagged::gc::{Mutator, OwnerPublication};
use std::num::NonZeroU64;

fn escape(mut mutator: Mutator<'static>, epoch: NonZeroU64) -> OwnerPublication<'static, 'static> {
    match mutator.begin_publication(epoch) {
        Ok(publication) => publication,
        Err(_) => panic!("fixture publication failed"),
    }
}

fn main() {}
