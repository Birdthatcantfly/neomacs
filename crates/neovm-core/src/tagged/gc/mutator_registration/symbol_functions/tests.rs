//! The actual minimal retained World; no bootstrap or setup collection.
use super::*;
use crate::emacs_core::eval::Context;
use crate::tagged::gc::{Mutator, PlainSymbolBacking, RendezvousError};
use crate::tagged::transport::SharedRoot;
use crate::tagged::value::TaggedValue as Value;
use std::cell::RefCell;
use std::rc::Rc;

struct Homes<'era> {
    symbol: crate::emacs_core::intern::SymId,
    old: OwnerRootSlot<'era>,
    new: OwnerRootSlot<'era>,
    dst: OwnerRootSlot<'era>,
    cache: OwnerRootSlot<'era>,
    activation: OwnerRootSlot<'era>,
}
struct FixtureGuard<'era> {
    owner: Option<Mutator<'era>>,
    input: Option<SharedRoot>,
    homes: Option<Homes<'era>>,
    cleanup: Rc<RefCell<Option<Result<(), String>>>>,
}
impl Drop for FixtureGuard<'_> {
    fn drop(&mut self) {
        // Total, nonpanicking cleanup records errors separately from the
        // tested body. Every nested lease/RunAccess has already unwound.
        let cleanup = (|| {
            drop(self.homes.take());
            let mut owner = self.owner.take().ok_or("C4 owner absent")?;
            owner
                .run_access()
                .map_err(|e| format!("C4 cleanup run: {e:?}"))?
                .clear_root_slots()
                .map_err(|e| format!("C4 cleanup clear: {e:?}"))?;
            drop(self.input.take());
            for _ in 0..10_000 {
                match owner.retire_rooted(&[]) {
                    Ok(()) => return Ok(()),
                    Err(failed) if failed.reason == RendezvousError::Busy => {
                        owner = failed.into_owner();
                        std::thread::yield_now();
                    }
                    Err(failed) => return Err(format!("C4 cleanup retirement: {failed:?}")),
                }
            }
            Err("C4 cleanup retirement remained Busy".into())
        })();
        *self.cleanup.borrow_mut() = Some(cleanup);
    }
}
#[derive(Clone, Copy)]
enum InitialFunction {
    Bound,
    Empty,
    Unbound,
}
fn fixture(
    kind: PlainSymbolBacking,
    body: impl for<'owner, 'era> FnOnce(&mut RunAccess<'owner, 'era>, &[SymbolHandle], &mut Homes<'era>),
) -> std::thread::Result<()> {
    fixture_state(kind, InitialFunction::Bound, body)
}
fn fixture_state(
    kind: PlainSymbolBacking,
    initial: InitialFunction,
    body: impl for<'owner, 'era> FnOnce(&mut RunAccess<'owner, 'era>, &[SymbolHandle], &mut Homes<'era>),
) -> std::thread::Result<()> {
    Context::with_test_symbol_functions_init_era(
        kind,
        |obarray, ids| match initial {
            InitialFunction::Bound => {}
            InitialFunction::Empty => obarray.clear_function_silent_id(ids[0]),
            InitialFunction::Unbound => obarray.fmakunbound_id(ids[0]),
        },
        |era, ids| {
            let handles: Vec<_> = ids
                .iter()
                .map(|&id| era.admit_plain_symbol(id).unwrap())
                .collect();
            let mut stopped = era.stop().unwrap();
            let input = era
                .with_stopped_heap_for_test(&stopped, |heap| unsafe {
                    // SAFETY: scalar setup on the actual matching stopped heap.
                    SharedRoot::new(heap, Value::NIL)
                })
                .unwrap();
            stopped.trace(|_, _| Ok(())).unwrap();
            stopped.complete().unwrap();
            let owner = era
                .register(1, 8)
                .unwrap()
                .activate(era.heap_identity(), 1)
                .unwrap();
            let cleanup = Rc::new(RefCell::new(None));
            let receipt = Rc::clone(&cleanup);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let mut guard = FixtureGuard {
                    owner: Some(owner),
                    input: Some(input),
                    homes: None,
                    cleanup: receipt,
                };
                let mut run = guard.owner.as_mut().unwrap().run_access().unwrap();
                let input = guard.input.as_ref().unwrap();
                guard.homes = Some(Homes {
                    symbol: ids[0],
                    old: run.root_slot(input).unwrap(),
                    new: run.root_slot(input).unwrap(),
                    dst: run.root_slot(input).unwrap(),
                    cache: run.root_slot(input).unwrap(),
                    activation: run.root_slot(input).unwrap(),
                });
                let homes = guard.homes.as_mut().unwrap();
                // Existing C3 reads supply actual immutable definition operands.
                run.sym_value_into(&handles[0], &mut homes.old).unwrap();
                run.sym_value_into(&handles[1], &mut homes.new).unwrap();
                assert!(Value::from_bits(homes.old.word_for_test(&run)).is_cons());
                assert!(Value::from_bits(homes.new.word_for_test(&run)).is_cons());
                body(&mut run, &handles, homes);
            }));
            assert_eq!(
                cleanup.borrow().as_ref(),
                Some(&Ok(())),
                "C4 fixture teardown failed independently of op outcome"
            );
            let (collector, keeper) = era
                .finish()
                .expect("C4 era finish after complete fixture cleanup");
            drop(collector);
            drop(keeper);
            eprintln!("P3SYM_C4_TEARDOWN_OK {kind:?}");
            outcome
        },
    )
}
fn oracle() -> Vec<&'static str> {
    let rows: Vec<_> = include_str!("../symbol_functions_oracle.expected")
        .lines()
        .collect();
    assert_eq!(rows.len(), 20, "GNU C4 oracle rows");
    rows
}
fn primitive(kind: PlainSymbolBacking, op: FunctionOpKind) {
    fixture(kind, |run, handles, homes| {
        let outcome = match op {
            FunctionOpKind::Function => run.sym_function_into(&handles[0], &mut homes.dst),
            FunctionOpKind::Fset => run.sym_fset_into(&handles[0], &homes.new, &mut homes.dst),
            FunctionOpKind::Fmakunbound => run.sym_fmakunbound_into(&handles[0], &mut homes.dst),
            FunctionOpKind::CallBegin => {
                let mut cache = FunctionCallCache::empty(homes.cache.clone());
                let mut activation = FunctionActivation::empty(homes.activation.clone());
                run.sym_call_begin(&handles[0], &mut cache, &mut activation)
                    .and_then(|()| {
                        for _ in 0..4 {
                            if run.sym_call_step(
                                &mut activation,
                                &mut homes.dst,
                                NonZeroU32::MIN,
                            )? == FunctionStep::Returned
                            {
                                return Ok(());
                            }
                        }
                        Err(FunctionError::UnsupportedDefinition)
                    })
            }
            _ => panic!("not a primitive fixture op"),
        };
        if let Err(FunctionError::MissingOperation(actual)) = outcome {
            // Prove the exact missing op before the intentional red assertion;
            // then unwind this RunAccess before FixtureGuard clears the bank.
            assert_eq!(actual, op, "C4 missing-op identity");
            panic!(
                "P3SYM_C4_RED missing executor op {}: {outcome:?}",
                op.method()
            );
        }
        assert_eq!(outcome, Ok(()), "unexpected C4 operation outcome");
        let answer = homes.dst.word_for_test(run);
        let (value, expected) = match op {
            FunctionOpKind::Function => (
                Value::bool_val(answer == homes.old.word_for_test(run)),
                oracle()[0],
            ),
            FunctionOpKind::Fset => (
                Value::bool_val(answer == homes.new.word_for_test(run)),
                oracle()[1],
            ),
            // The returned identity must be exactly the original target.
            FunctionOpKind::Fmakunbound => (
                Value::bool_val(answer == Value::from_sym_id(homes.symbol).bits()),
                oracle()[2],
            ),
            FunctionOpKind::CallBegin => (Value::from_bits(answer), oracle()[3]),
            _ => unreachable!(),
        };
        assert_eq!(crate::emacs_core::print_value(&value), expected);
    })
    .expect("C4 primitive twin after independent clean teardown");
}
#[test]
fn p3sym_c4_noop_fixture_releases_roots_and_finishes() {
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        fixture(kind, |_, _, _| {}).expect("C4 no-op fixture completion");
    }
}
macro_rules! red {
    ($name:ident, $kind:ident, $op:ident) => {
        #[test]
        #[ignore = "C4 preparation: must fail on the named missing-op assertion"]
        fn $name() {
            primitive(PlainSymbolBacking::$kind, FunctionOpKind::$op);
        }
    };
}
#[test]
fn p3sym_c4_plain_function() {
    primitive(PlainSymbolBacking::Unprojected, FunctionOpKind::Function);
}
red!(p3sym_c4_red_plain_fset, Unprojected, Fset);
red!(p3sym_c4_red_plain_fmakunbound, Unprojected, Fmakunbound);
red!(p3sym_c4_red_plain_call, Unprojected, CallBegin);
#[test]
fn p3sym_c4_projected_function() {
    primitive(PlainSymbolBacking::Projected, FunctionOpKind::Function);
}
red!(p3sym_c4_red_projected_fset, Projected, Fset);
red!(p3sym_c4_red_projected_fmakunbound, Projected, Fmakunbound);
red!(p3sym_c4_red_projected_call, Projected, CallBegin);

#[test]
fn p3sym_c4_surface_preserves_owner_root_and_fuel_types() {
    let surface = include_str!("../symbol_functions.rs");
    for forbidden in [
        "-> Value",
        "-> TaggedValue",
        "-> usize",
        "unsafe {",
        "expect(",
        "unwrap(",
    ] {
        assert!(
            !surface.contains(forbidden),
            "C4 surface exposed {forbidden}"
        );
    }
    assert!(surface.contains("fuel: NonZeroU32"));
    assert!(surface.contains("impl KeeperSymbols"));
}

#[test]
fn p3sym_c4_function_empty_and_unbound_are_nil() {
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        for (initial, row, field) in [
            (InitialFunction::Empty, 16, 0),
            (InitialFunction::Unbound, 17, 1),
        ] {
            fixture_state(kind, initial, |run, handles, homes| {
                run.sym_function_into(&handles[0], &mut homes.dst).unwrap();
                // Select only symbol-function's scalar field from the pinned
                // GNU rows (fboundp and the unbind identity are separate fields).
                let expected = oracle()[row]
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .split_whitespace()
                    .nth(field)
                    .unwrap();
                assert_eq!(
                    crate::emacs_core::print_value(&Value::from_bits(homes.dst.word_for_test(run))),
                    expected
                );
            })
            .expect("GNU nil function-cell state and cleanup");
        }
    }
}

#[test]
fn p3sym_c4_function_invalid_and_cleared_homes_refuse() {
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        fixture(kind, |run, handles, homes| {
            let before = homes.dst.word_for_test(run);
            let mut invalid = homes.dst.clone();
            invalid.index = usize::MAX;
            assert_eq!(
                run.sym_function_into(&handles[0], &mut invalid),
                Err(FunctionError::Authority(
                    crate::tagged::gc::RegistrationError::StaleRegistration
                ))
            );
            assert_eq!(
                homes.dst.word_for_test(run),
                before,
                "invalid destination preserves the real home"
            );
            run.sym_function_into(&handles[0], &mut homes.dst).unwrap();
            assert_eq!(homes.dst.word_for_test(run), homes.old.word_for_test(run));
            run.clear_root_slots().unwrap();
            assert_eq!(
                run.sym_function_into(&handles[0], &mut homes.dst),
                Err(FunctionError::Authority(
                    crate::tagged::gc::RegistrationError::StaleRegistration
                ))
            );
            assert!(run.owner.local.as_ref().unwrap().root_scratch.is_empty());
        })
        .expect("total function-home validation and cleanup");
    }
}

#[test]
fn p3sym_c4_function_read_has_no_fatal_decode_or_value_escape() {
    let source = include_str!("../symbol_authority/functions.rs");
    for forbidden in [
        ".expect(",
        ".unwrap(",
        "panic!(",
        "unreachable!(",
        "-> Value",
        "-> TaggedValue",
        "-> usize",
    ] {
        assert!(
            !source.contains(forbidden),
            "function read exposed {forbidden}"
        );
    }
    assert!(source.contains("check_home(self, dst)?"));
    assert!(source.contains("symbol_ref(symbol)?"));
    assert!(source.contains("self.read_token()"));
}
