use neovm_core::tagged::gc::NativeRoots;

fn consume<T>(value: T) {
    drop(value);
}

fn transfer(roots: NativeRoots<'static>) {
    std::thread::spawn(move || consume(roots));
}

fn main() {}
