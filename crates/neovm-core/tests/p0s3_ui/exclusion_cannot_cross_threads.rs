use neovm_core::tagged::gc::FacadeMarkExclusion;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(exclusion: FacadeMarkExclusion) {
    ThreadTransfer::<FacadeMarkExclusion>::transfer(exclusion);
}

fn main() {}
