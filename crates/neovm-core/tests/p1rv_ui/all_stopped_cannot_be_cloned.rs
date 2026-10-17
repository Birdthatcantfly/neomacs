use neovm_core::tagged::gc::AllStopped;

fn duplicate(stopped: AllStopped<'static>) {
    let second = stopped.clone();
    drop((stopped, second));
}

fn main() {}
