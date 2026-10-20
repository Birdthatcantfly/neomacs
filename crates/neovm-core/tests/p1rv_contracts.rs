//! Compiler contracts for real parallel mutator registration authorities.

#[test]
#[ignore = "compiles separate fixtures; run explicitly with light gates"]
fn p1rv_compile_contracts() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/p1rv_ui/*.rs");
}

#[test]
#[ignore = "compiles proof-3 fixtures in the gate toolchain"]
fn p3sym_compile_contracts() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/p1rv_ui/p3sym_*.rs");
}
