use neovm_core::tagged::gc::ResumeGcImage;

fn duplicate(image: ResumeGcImage<'static, 'static>) {
    let second = image.clone();
    drop((image, second));
}

fn main() {}
