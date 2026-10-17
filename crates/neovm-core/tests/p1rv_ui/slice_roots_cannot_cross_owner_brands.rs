use neovm_core::tagged::gc::{OwnerRootSlot, RunAccess};

fn foreign_car(
    first: &mut RunAccess<'_, '_>,
    second: &mut RunAccess<'_, '_>,
    first_home: &OwnerRootSlot<'_>,
    second_home: &OwnerRootSlot<'_>,
) {
    let _ = first.with_slice(|left| {
        let foreign = left.bind(first_home).unwrap();
        let _ = second.with_slice(|right| {
            let local = right.bind(second_home).unwrap();
            let mut result = local.clone();
            let _ = right.cons_into(&foreign, &local, &mut result);
        });
    });
}

fn foreign_cdr(
    first: &mut RunAccess<'_, '_>,
    second: &mut RunAccess<'_, '_>,
    first_home: &OwnerRootSlot<'_>,
    second_home: &OwnerRootSlot<'_>,
) {
    let _ = first.with_slice(|left| {
        let foreign = left.bind(first_home).unwrap();
        let _ = second.with_slice(|right| {
            let local = right.bind(second_home).unwrap();
            let mut result = local.clone();
            let _ = right.cons_into(&local, &foreign, &mut result);
        });
    });
}

fn foreign_result(
    first: &mut RunAccess<'_, '_>,
    second: &mut RunAccess<'_, '_>,
    first_home: &OwnerRootSlot<'_>,
    second_home: &OwnerRootSlot<'_>,
) {
    let _ = first.with_slice(|left| {
        let mut foreign = left.bind(first_home).unwrap();
        let _ = second.with_slice(|right| {
            let local = right.bind(second_home).unwrap();
            let _ = right.cons_into(&local, &local, &mut foreign);
        });
    });
}

fn main() {}
