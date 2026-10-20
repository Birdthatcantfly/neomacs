use neovm_core::tagged::gc::SymbolMut;
fn require_send<T: Send>() {}
fn main() {
    require_send::<SymbolMut<'static, 'static, 'static>>();
}
