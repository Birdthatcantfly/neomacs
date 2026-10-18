//! Leases exist only as mints from a run access or stop-all handle; the
//! struct literal path is closed by private fields.

use neovm_core::tagged::gc::TableMut;

fn forge() -> TableMut<'static> {
    TableMut {}
}

fn main() {}
