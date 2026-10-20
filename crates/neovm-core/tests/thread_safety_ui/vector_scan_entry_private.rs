use neovm_core::tagged::header::VectorScanEntry;

fn main() {
    let _entry = VectorScanEntry::Owned {
        base: std::ptr::null(),
        len: 1,
    };
}
