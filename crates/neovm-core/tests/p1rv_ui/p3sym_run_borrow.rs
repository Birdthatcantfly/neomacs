use neovm_core::tagged::gc::{RunAccess, SymbolHandle};
fn release_while_reading(mut run: RunAccess<'_, '_>, key: SymbolHandle) {
    let reader = run.symbol_ref(&key).unwrap();
    drop(run);
    let _ = reader.backing();
}
fn main() {}
