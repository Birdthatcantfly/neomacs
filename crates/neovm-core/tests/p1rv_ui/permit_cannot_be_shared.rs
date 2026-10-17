use neovm_core::tagged::gc::MutatorPermit;

fn consume<T>(value: T) {
    drop(value);
}

fn share(permit: &MutatorPermit<'_>) {
    std::thread::scope(|scope| {
        scope.spawn(move || consume(permit));
    });
}

fn main() {}
