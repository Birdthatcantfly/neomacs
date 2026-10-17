use neovm_core::tagged::gc::Mutator;
use std::num::NonZeroU64;

fn consume<T>(value: T) {
    drop(value);
}

fn publish_while_running(mutator: &mut Mutator<'_>, epoch: NonZeroU64) {
    let running = match mutator.run_access() {
        Ok(running) => running,
        Err(_) => panic!("fixture run access failed"),
    };
    let publication = mutator.begin_publication(epoch);
    consume((running, publication));
}

fn main() {}
