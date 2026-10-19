use neovm_core::tagged::gc::ResumeGcImage;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(image: ResumeGcImage<'static, 'static>) {
    ThreadTransfer::<ResumeGcImage<'static, 'static>>::transfer(image);
}

fn main() {}
