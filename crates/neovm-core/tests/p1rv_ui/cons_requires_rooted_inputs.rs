use neovm_core::tagged::gc::RunAccess;
use neovm_core::tagged::value::TaggedValue;

fn consume<T>(value: T) {
    drop(value);
}

fn unrooted_operands(run: &mut RunAccess<'_, '_>, car: TaggedValue, cdr: TaggedValue) {
    consume(run.cons(&car, &cdr));
}

fn main() {}
