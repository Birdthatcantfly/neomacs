use neovm_core::tagged::gc::NativeRoots;

fn assert_copy<T: Copy>() {}

fn main() {
    assert_copy::<NativeRoots<'static>>();
}
