use neovm_core::tagged::gc::AllStopped;

fn consume<T>(value: T) {
    drop(value);
}

fn transfer(stopped: AllStopped<'static>) {
    std::thread::spawn(move || consume(stopped));
}

fn main() {}
