use neovm_core::tagged::gc::OwnerRootSlot;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(slot: OwnerRootSlot<'static>) {
    ThreadTransfer::<OwnerRootSlot<'static>>::transfer(slot);
}

fn main() {}
