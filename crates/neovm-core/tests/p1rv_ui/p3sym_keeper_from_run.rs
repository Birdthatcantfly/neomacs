use neovm_core::tagged::gc::RunAccess;
fn forge(run: &mut RunAccess<'_, '_>) {
    let _ = run.symbols();
}
fn main() {}
