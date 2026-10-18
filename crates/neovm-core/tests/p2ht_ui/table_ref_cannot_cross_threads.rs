//! A reader lease is pinned to the owning thread that minted it.

use neovm_core::tagged::gc::TableRef;

fn main() {
    fn cross(lease: TableRef<'static>) {
        std::thread::spawn(move || drop(lease));
    }
    let _ = cross;
}
