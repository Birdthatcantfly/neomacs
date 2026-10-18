//! The table-access capability cannot be extended outside the lease
//! module: implementing it requires the private seal supertrait.

use neovm_core::tagged::gc::SealedTableAccess;

struct ForgedTableAccess;

impl SealedTableAccess for ForgedTableAccess {}

fn main() {}
