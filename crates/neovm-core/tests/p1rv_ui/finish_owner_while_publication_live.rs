use neovm_core::tagged::gc::Mutator;
use std::num::NonZeroU64;

fn consume<T>(value: T) {
    drop(value);
}

fn finish_while_borrowed(mut mutator: Mutator<'_>, epoch: NonZeroU64) {
    let publication = match mutator.begin_publication(epoch) {
        Ok(publication) => publication,
        Err(_) => panic!("fixture publication failed"),
    };
    let finished = mutator.finish(epoch);
    consume(publication.publish());
    consume(finished);
}

fn main() {}
