use neovm_core::tagged::gc::NativeRoots;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(roots: NativeRoots<'static>) {
    ThreadTransfer::<NativeRoots<'static>>::transfer(roots);
}

fn main() {}
