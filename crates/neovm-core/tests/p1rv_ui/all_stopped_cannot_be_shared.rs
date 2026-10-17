use neovm_core::tagged::gc::AllStopped;

fn require_sync<T: Sync>() {}

fn main() {
    require_sync::<AllStopped<'static>>();
}
