use neovm_core::tagged::gc::Mutator;
use neovm_core::tagged::value::TaggedValue;

fn consume<T>(value: T) {
    drop(value);
}

fn allocate_through_whole_heap(mutator: &mut Mutator<'_>) {
    consume(mutator.alloc_cons(TaggedValue::NIL, TaggedValue::NIL));
}

fn main() {}
