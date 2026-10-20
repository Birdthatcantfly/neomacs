use neovm_core::tagged::gc::{RunAccess, SymbolHandle, SymbolMut};
fn forge<'a, 'owner, 'era>(
    run: &'a mut RunAccess<'owner, 'era>, key: SymbolHandle,
) -> SymbolMut<'a, 'owner, 'era> {
    SymbolMut { run, key }
}
fn main() {}
