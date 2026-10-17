use neovm_core::tagged::gc::Mutator;

fn consume<T>(value: T) {
    drop(value);
}

fn transfer(mutator: Mutator<'static>) {
    std::thread::spawn(move || consume(mutator));
}

fn main() {}
