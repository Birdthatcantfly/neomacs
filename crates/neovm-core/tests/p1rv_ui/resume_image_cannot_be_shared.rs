use neovm_core::tagged::gc::ResumeGcImage;

fn require_sync<T: Sync>() {}

fn main() {
    require_sync::<ResumeGcImage<'static, 'static>>();
}
