use neovm_core::tagged::gc::ResumeGcImage;

fn consume<T>(value: T) {
    drop(value);
}

fn transfer(image: ResumeGcImage<'static, 'static>) {
    std::thread::spawn(move || consume(image));
}

fn main() {}
