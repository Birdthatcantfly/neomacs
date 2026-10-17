use neovm_core::tagged::gc::Mutator;
use neovm_core::tagged::transport::SharedRoot;
use std::num::NonZeroU64;

fn consume<T>(value: T) {
    drop(value);
}

fn finish_after_region_close(
    mut owner: Mutator<'_>,
    epoch: NonZeroU64,
    car: &SharedRoot,
    cdr: &SharedRoot,
) {
    let mut running = owner.run_access().expect("fixture run access failed");
    running
        .close_cons_region()
        .expect("fixture region close failed");
    let finished = owner.finish(epoch);
    consume((finished, running.cons(car, cdr)));
}

fn main() {}
