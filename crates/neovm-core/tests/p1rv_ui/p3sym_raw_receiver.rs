use neovm_core::tagged::gc::SymbolMut;
fn bypass(writer: &mut SymbolMut<'_, '_, '_>) {
    let _ = writer.obarray_mut();
}
fn main() {}
