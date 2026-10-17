use neovm_core::tagged::gc::ParallelHeapEra;
use neovm_core::tagged::value::TaggedValue;

fn consume<T>(value: T) {
    drop(value);
}

fn allocate_through_whole_heap(era: &mut ParallelHeapEra<'_>) {
    consume(era.alloc_cons(TaggedValue::NIL, TaggedValue::NIL));
}

fn main() {}
