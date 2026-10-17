use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess};

fn use_slice_after_dropping_run(mut run: RunAccess<'_, '_>, home: &OwnerRootSlot<'_>) {
    let _ = run.with_slice(|slice| {
        drop(run);
        let car = slice.bind(home).unwrap();
        let cdr = car.clone();
        let mut result = car.clone();
        let _ = slice.cons_into(&car, &cdr, &mut result);
    });
}

fn main() {}
