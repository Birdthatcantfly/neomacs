//! Small Miri regression for the obarray's GC-visible function/plist words.

use super::*;

#[test]
fn miriub3_gc_collect_exact_traces_function_and_plist() {
    // Deliberately omit builtin/bootstrap registration: only the reduced VM
    // roots and this symbol are needed to reach Obarray::trace_roots.
    let mut context = Context::new_minimal_vm_harness();
    let symbol = intern("miriub3-function-and-plist");
    let function = Value::cons(Value::fixnum(71), Value::NIL);
    let plist = Value::cons(Value::fixnum(83), Value::NIL);
    context.obarray.set_symbol_function_id(symbol, function);
    context.obarray.set_symbol_plist_id(symbol, plist);

    context.gc_collect_exact();

    let function = context.obarray.symbol_function_id(symbol).unwrap();
    let plist = context.obarray.symbol_plist_id(symbol);
    assert_eq!(function.cons_car(), Value::fixnum(71));
    assert_eq!(plist.cons_car(), Value::fixnum(83));
}
