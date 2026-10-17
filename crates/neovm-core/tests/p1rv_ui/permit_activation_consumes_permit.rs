use neovm_core::tagged::gc::{HeapIdentity, MutatorPermit};

fn consume<T>(value: T) {
    drop(value);
}

fn activate_twice(permit: MutatorPermit<'_>, heap: HeapIdentity, owner: u64) {
    let first = permit.activate(heap, owner);
    let second = permit.activate(heap, owner);
    consume((first, second));
}

fn main() {}
