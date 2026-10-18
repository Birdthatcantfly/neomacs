use neovm_core::tagged::gc::NativeRoots;

fn assert_sync<T: Sync>() {}

fn main() {
    assert_sync::<NativeRoots<'static>>();
}
