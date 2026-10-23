use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess, SymbolHandle};
use neovm_core::tagged::value::TaggedValue;

fn export_definition<'owner, 'era>(
    run: &mut RunAccess<'owner, 'era>,
    symbol: &SymbolHandle,
    home: &mut OwnerRootSlot<'era>,
) -> TaggedValue {
    run.sym_function_into(symbol, home).unwrap()
}

fn main() {}
