//! The collector's stop-all license never leaves the keeper thread.

use neovm_core::tagged::gc::KeeperTableMut;

fn main() {
    fn cross(license: KeeperTableMut<'static>) {
        std::thread::spawn(move || drop(license));
    }
    let _ = cross;
}
