use neovm_core::tagged::gc::ResumeGcImage;

fn fabricate() -> ResumeGcImage<'static, 'static> {
    ResumeGcImage {}
}

fn main() {}
