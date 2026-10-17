use neovm_core::tagged::gc::OwnerPublication;

fn consume<T>(value: T) {
    drop(value);
}

fn publish_twice(publication: OwnerPublication<'_, '_>) {
    let first = publication.publish();
    let second = publication.publish();
    consume((first, second));
}

fn main() {}
