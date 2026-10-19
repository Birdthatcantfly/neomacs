use neovm_core::tagged::transport::RootBatchReader;

#[path = "../thread_safety_ui/support/thread_bounds.rs"]
mod thread_bounds;
use thread_bounds::ThreadTransfer;

fn transfer(reader: RootBatchReader<'static>) {
    ThreadTransfer::<RootBatchReader<'static>>::transfer(reader);
}

fn main() {}
