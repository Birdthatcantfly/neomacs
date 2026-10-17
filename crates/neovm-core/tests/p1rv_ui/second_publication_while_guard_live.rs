use neovm_core::tagged::gc::Mutator;
use std::num::NonZeroU64;

fn consume<T>(value: T) {
    drop(value);
}

fn publish_twice(mutator: &mut Mutator<'_>, epoch: NonZeroU64) {
    let first = match mutator.begin_publication(epoch) {
        Ok(publication) => publication,
        Err(_) => panic!("fixture publication failed"),
    };
    let second = mutator.begin_publication(epoch);
    consume((first, second));
}

fn main() {}
