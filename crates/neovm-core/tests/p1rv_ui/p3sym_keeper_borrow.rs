use neovm_core::tagged::gc::{AllStopped, SymbolHandle};
fn resume_while_keeping(mut stopped: AllStopped<'_>, key: SymbolHandle) {
    let keeper = stopped.symbols().unwrap();
    let _ = stopped.complete();
    let _ = keeper.backing(&key);
}
fn main() {}
