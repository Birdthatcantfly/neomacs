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
    Exhausted,
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
    fixture_config(kind, initial, true, body)
}
fn fixture_config(
    kind: PlainSymbolBacking,
    initial: InitialFunction,
    prepare: bool,
    body: impl for<'owner, 'era> FnOnce(&mut RunAccess<'owner, 'era>, &[SymbolHandle], &mut Homes<'era>),
) -> std::thread::Result<()> {
    Context::with_test_symbol_functions_init_era(
        kind,
        |obarray, ids| match initial {
            InitialFunction::Bound => {}
            InitialFunction::Empty => obarray.clear_function_silent_id(ids[0]),
            InitialFunction::Unbound => obarray.fmakunbound_id(ids[0]),
            InitialFunction::Exhausted => obarray.set_function_epoch_for_test(u64::MAX - 1),
        },
        |era, ids| {
            let handles: Vec<_> = ids
                .iter()
                .map(|&id| era.admit_plain_symbol(id).unwrap())
                .collect();
            if prepare {
                for key in &handles {
                    era.prepare_function_symbol(key).unwrap();
                }
            }
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
            let pending = era.registry().symbols.functions.finish_ready().is_err();
            let era = if pending {
                let failure = era
                    .finish()
                    .expect_err("dirty function era must refuse serial return");
                assert_eq!(
                    failure.reason,
                    crate::tagged::gc::RegistrationError::PendingRoots
                );
                failure.era
            } else {
                era
            };
            let mut stopped = era.stop().unwrap();
            let mut symbols = stopped.symbols().unwrap();
            let mut journal = Vec::new();
            symbols.append_function_roots(&mut journal).unwrap();
            assert_eq!(
                !journal.is_empty(),
                pending,
                "original-target root obligation"
            );
            symbols.reconcile_functions().unwrap();
            journal.clear();
            symbols.append_function_roots(&mut journal).unwrap();
            assert!(
                journal.is_empty(),
                "successful stopped reconciliation discharged originals"
            );
            drop(symbols);
            stopped.trace(|_, _| Ok(())).unwrap();
            stopped.complete().unwrap();
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
#[test]
fn p3sym_c4_plain_fset() {
    primitive(PlainSymbolBacking::Unprojected, FunctionOpKind::Fset);
}
red!(p3sym_c4_red_plain_fmakunbound, Unprojected, Fmakunbound);
red!(p3sym_c4_red_plain_call, Unprojected, CallBegin);
#[test]
fn p3sym_c4_projected_function() {
    primitive(PlainSymbolBacking::Projected, FunctionOpKind::Function);
}
#[test]
fn p3sym_c4_projected_fset() {
    primitive(PlainSymbolBacking::Projected, FunctionOpKind::Fset);
}
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
    assert!(include_str!("../symbol_authority/functions/write.rs").contains("impl KeeperSymbols"));
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

#[test]
fn p3sym_c4_fset_noop_and_two_symbol_clock() {
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        fixture(kind, |run, handles, homes| {
            let left = run.function_facts_for_test(&handles[0]).unwrap();
            let right = run.function_facts_for_test(&handles[1]).unwrap();
            run.sym_fset_into(&handles[0], &homes.old, &mut homes.dst)
                .unwrap();
            assert_eq!(run.function_facts_for_test(&handles[0]).unwrap(), left);
            assert_eq!(homes.dst.word_for_test(run), homes.old.word_for_test(run));
            run.sym_fset_into(&handles[0], &homes.new, &mut homes.dst)
                .unwrap();
            let first = run.function_facts_for_test(&handles[0]).unwrap();
            assert_eq!(first, (left.0 + 1, left.0 + 1, true));
            let unrelated = run.function_facts_for_test(&handles[1]).unwrap();
            assert_eq!(unrelated, (first.0, right.1, true));
            run.sym_fset_into(&handles[1], &homes.old, &mut homes.dst)
                .unwrap();
            let second = run.function_facts_for_test(&handles[1]).unwrap();
            assert_eq!(second, (first.0 + 1, first.0 + 1, true));
            assert_eq!(run.function_facts_for_test(&handles[0]).unwrap().1, first.1);
            run.sym_function_into(&handles[0], &mut homes.dst).unwrap();
            let literal = |bits| {
                let value = Value::from_bits(bits);
                crate::emacs_core::print_value(
                    &value
                        .cons_cdr_unobserved()
                        .cons_cdr_unobserved()
                        .cons_car_unobserved(),
                )
            };
            let a = literal(homes.dst.word_for_test(run));
            run.sym_function_into(&handles[1], &mut homes.dst).unwrap();
            let b = literal(homes.dst.word_for_test(run));
            assert_eq!(format!("({a} {b})"), oracle()[11]);
        })
        .expect("two-symbol function publication and stopped discharge");
    }
}

#[test]
fn p3sym_c4_fset_nil_distinguishes_empty_and_explicit_unbound() {
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        for initial in [InitialFunction::Empty, InitialFunction::Unbound] {
            fixture_state(kind, initial, |run, handles, homes| {
                run.sym_function_into(&handles[0], &mut homes.dst).unwrap();
                let before = run.function_facts_for_test(&handles[0]).unwrap();
                let nil = homes.dst.clone();
                run.sym_fset_into(&handles[0], &nil, &mut homes.dst)
                    .unwrap();
                let after = run.function_facts_for_test(&handles[0]).unwrap();
                let changed = matches!(initial, InitialFunction::Unbound);
                assert_eq!(after.0, before.0 + u64::from(changed));
                assert_eq!(after.2, changed);
                assert_eq!(
                    crate::emacs_core::print_value(&Value::from_bits(homes.dst.word_for_test(run))),
                    oracle()[18].split_whitespace().nth(1).unwrap()
                );
                run.sym_fset_into(&handles[0], &nil, &mut homes.dst)
                    .unwrap();
                assert_eq!(run.function_facts_for_test(&handles[0]).unwrap(), after);
            })
            .expect("nil transition preserves exact unbound state and teardown");
        }
    }
}

#[test]
fn p3sym_c4_fset_exhaustion_allows_only_noop() {
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        fixture_state(kind, InitialFunction::Exhausted, |run, handles, homes| {
            let before = run.function_facts_for_test(&handles[0]).unwrap();
            run.sym_fset_into(&handles[0], &homes.old, &mut homes.dst)
                .unwrap();
            let answer = homes.dst.word_for_test(run);
            assert_eq!(
                run.sym_fset_into(&handles[0], &homes.new, &mut homes.dst),
                Err(FunctionError::EpochExhausted)
            );
            assert_eq!(run.function_facts_for_test(&handles[0]).unwrap(), before);
            assert_eq!(homes.dst.word_for_test(run), answer);
            run.sym_function_into(&handles[0], &mut homes.dst).unwrap();
            assert_eq!(homes.dst.word_for_test(run), homes.old.word_for_test(run));
        })
        .expect("epoch exhaustion refuses before mutation or journal retention");
    }
}

#[test]
fn p3sym_c4_fset_refuses_unprepared_unsupported_and_stale_homes() {
    use crate::tagged::gc::{RegistrationError, mark_word::MarkWord};
    for kind in [
        PlainSymbolBacking::Unprojected,
        PlainSymbolBacking::Projected,
    ] {
        fixture_config(
            kind,
            InitialFunction::Bound,
            false,
            |run, handles, homes| {
                let before = run.function_facts_for_test(&handles[0]).unwrap();
                let answer = homes.dst.word_for_test(run);
                assert_eq!(
                    run.sym_fset_into(&handles[0], &homes.new, &mut homes.dst),
                    Err(FunctionError::Authority(RegistrationError::Publication))
                );
                assert_eq!(run.function_facts_for_test(&handles[0]).unwrap(), before);
                assert_eq!(homes.dst.word_for_test(run), answer);
            },
        )
        .expect("missing preparation refuses without mutation");
        fixture(kind, |run, handles, homes| {
            let before = run.function_facts_for_test(&handles[0]).unwrap();
            let answer = homes.dst.word_for_test(run);
            for bad in [Value::UNBOUND, Value::fixnum(123)] {
                run.owner.local.as_mut().unwrap().root_scratch[homes.cache.index] =
                    MarkWord::of(bad);
                assert_eq!(
                    run.sym_fset_into(&handles[0], &homes.cache, &mut homes.dst),
                    Err(FunctionError::UnsupportedDefinition)
                );
                assert_eq!(run.function_facts_for_test(&handles[0]).unwrap(), before);
                assert_eq!(homes.dst.word_for_test(run), answer);
            }
            let mut invalid = homes.dst.clone();
            invalid.index = usize::MAX;
            assert_eq!(
                run.sym_fset_into(&handles[0], &homes.new, &mut invalid),
                Err(FunctionError::Authority(
                    RegistrationError::StaleRegistration
                ))
            );
            assert_eq!(run.function_facts_for_test(&handles[0]).unwrap(), before);
        })
        .expect("unsupported operands and stale homes preserve cell/version/answer");
    }
}
