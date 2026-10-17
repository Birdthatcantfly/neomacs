use neovm_core::tagged::gc::OwnerRootSlot;

fn consume<T>(value: T) {
    drop(value);
}

fn transfer(slot: OwnerRootSlot<'static>) {
    std::thread::spawn(move || consume(slot));
}

fn main() {}
