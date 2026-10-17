use neovm_core::tagged::gc::ParallelHeapEra;

fn consume<T>(value: T) {
    drop(value);
}

fn transfer(era: ParallelHeapEra<'static>) {
    std::thread::spawn(move || consume(era));
}

fn main() {}
