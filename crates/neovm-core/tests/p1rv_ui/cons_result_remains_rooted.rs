use neovm_core::tagged::gc::RunAccess;
use neovm_core::tagged::transport::SharedRoot;
use neovm_core::tagged::value::TaggedValue;

fn unrooted_result(run: &mut RunAccess<'_, '_>, car: &SharedRoot, cdr: &SharedRoot) -> TaggedValue {
    run.cons(car, cdr).expect("fixture cons allocation failed")
}

fn main() {}
