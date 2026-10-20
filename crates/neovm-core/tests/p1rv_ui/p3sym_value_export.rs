use neovm_core::tagged::gc::SymbolRef;
fn export(reader: &SymbolRef<'_, '_, '_>) {
    let _ = reader.value();
}
fn main() {}
