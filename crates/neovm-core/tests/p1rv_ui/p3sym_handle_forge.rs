use neovm_core::tagged::gc::SymbolHandle;
fn forge(key: &mut SymbolHandle) {
    key.world = 7;
}
fn main() {}
