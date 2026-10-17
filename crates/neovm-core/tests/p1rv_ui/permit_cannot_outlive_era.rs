use neovm_core::tagged::gc::{MutatorPermit, ParallelHeapEra};

fn escape(era: ParallelHeapEra<'static>) -> MutatorPermit<'static> {
    match era.register(1, 0) {
        Ok(permit) => permit,
        Err(_) => panic!("fixture registration failed"),
    }
}

fn main() {}
