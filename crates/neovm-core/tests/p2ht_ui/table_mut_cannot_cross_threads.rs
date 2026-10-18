//! A writer lease is pinned to the owning thread that minted it.

use neovm_core::tagged::gc::TableMut;

fn main() {
    fn cross(lease: TableMut<'static>) {
        std::thread::spawn(move || drop(lease));
    }
    let _ = cross;
}
