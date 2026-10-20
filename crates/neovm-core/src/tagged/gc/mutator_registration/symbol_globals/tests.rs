//! Explicit red semantics on real C2 handles; never run in a normal gate.
//! Run each ignored test and require its P3SYM_C3_RED assertion, not an
//! arbitrary failure. Teardown completes before the intentional assertion.
use super::*;
use crate::emacs_core::eval::Context;
use crate::tagged::gc::PlainSymbolBacking;
use crate::tagged::transport::SharedRoot;
use crate::tagged::value::TaggedValue as Value;

fn oracle() -> Vec<&'static str> {
    let lines: Vec<_> = include_str!("../symbol_globals_oracle.expected")
        .lines()
        .collect();
    assert_eq!(lines.len(), 16, "GNU 31.1 C3 fixture row count");
    lines
}

fn red_case(op: GlobalOp, kind: PlainSymbolBacking) {
    let expected = match op {
        GlobalOp::Value { .. } => oracle()[0],
        GlobalOp::Set { .. } => oracle()[1],
        GlobalOp::Boundp { .. } => oracle()[2],
        GlobalOp::Makunbound { .. } => oracle()[3],
    };
    let result = Context::with_test_symbol_backing_era(kind, |era, id| {
        let handle = era.admit_plain_symbol(id).unwrap();
        // Explicit matching stopped heap for serial immediate-root setup;
        // no collection, current-heap TLS access or raw global read.
        let mut stopped = era.stop().unwrap();
        let (input, output) = era
            .with_stopped_heap_for_test(&stopped, |heap| unsafe {
                // SAFETY: live scalar immediates on the matching stopped heap.
                (
                    SharedRoot::new(heap, Value::fixnum(97)),
                    SharedRoot::new(heap, Value::NIL),
                )
            })
            .unwrap();
        stopped.trace(|_, _| Ok(())).unwrap();
        stopped.complete().unwrap();
        let mut owner = era
            .register(1, 4)
            .unwrap()
            .activate(era.heap_identity(), 1)
            .unwrap();
        let result = {
            let mut run = owner.run_access().unwrap();
            assert_eq!(run.symbol_ref(&handle).unwrap().backing(), kind);
            let src = run.root_slot(&input).unwrap();
            let mut dst = run.root_slot(&output).unwrap();
            let outcome = match op {
                GlobalOp::Value { .. } => run.sym_value_into(&handle, &mut dst),
                GlobalOp::Set { .. } => run.sym_set_into(&handle, &src, &mut dst),
                GlobalOp::Boundp { .. } => run.sym_boundp_into(&handle, &mut dst),
                GlobalOp::Makunbound { .. } => run.sym_makunbound_into(&handle, &mut dst),
            };
            outcome.map(|()| {
                let value = Value::from_bits(dst.word_for_test(&run));
                let value = if matches!(op, GlobalOp::Makunbound { .. }) {
                    if value.bits() == Value::from_sym_id(id).bits() {
                        Value::T
                    } else {
                        Value::NIL
                    }
                } else {
                    value
                };
                crate::emacs_core::print_value(&value)
            })
        };
        drop(input);
        drop(output);
        owner.retire_rooted(&[]).unwrap();
        let (collector, keeper) = era.finish().unwrap();
        drop(collector);
        drop(keeper);
        result
    });
    assert_eq!(
        result.as_deref(),
        Ok(expected),
        "P3SYM_C3_RED {op:?} {kind:?}: storage implementation awaits b143"
    );
}

macro_rules! red {
    ($name:ident, $kind:ident, $op:expr) => {
        #[test]
        #[ignore = "C3 red semantics; real storage implementation waits for b143"]
        fn $name() {
            red_case($op, PlainSymbolBacking::$kind);
        }
    };
}

red!(
    p3sym_c3_red_plain_value,
    Unprojected,
    GlobalOp::Value { symbol: 0, dst: 1 }
);
red!(
    p3sym_c3_red_plain_set,
    Unprojected,
    GlobalOp::Set {
        symbol: 0,
        src: 0,
        dst: 1
    }
);
red!(
    p3sym_c3_red_plain_boundp,
    Unprojected,
    GlobalOp::Boundp { symbol: 0, dst: 1 }
);
red!(
    p3sym_c3_red_plain_makunbound,
    Unprojected,
    GlobalOp::Makunbound { symbol: 0, dst: 1 }
);
red!(
    p3sym_c3_red_projected_value,
    Projected,
    GlobalOp::Value { symbol: 0, dst: 1 }
);
red!(
    p3sym_c3_red_projected_set,
    Projected,
    GlobalOp::Set {
        symbol: 0,
        src: 0,
        dst: 1
    }
);
red!(
    p3sym_c3_red_projected_boundp,
    Projected,
    GlobalOp::Boundp { symbol: 0, dst: 1 }
);
red!(
    p3sym_c3_red_projected_makunbound,
    Projected,
    GlobalOp::Makunbound { symbol: 0, dst: 1 }
);

#[test]
fn p3sym_c3_api_is_test_only_and_slot_to_slot() {
    let api = include_str!("../symbol_globals.rs");
    for forbidden in [
        "-> Value",
        "-> TaggedValue",
        "-> usize",
        "pub trait",
        "unsafe {",
    ] {
        assert!(
            !api.contains(forbidden),
            "C3 preparation exports {forbidden}"
        );
    }
    let parent = include_str!("../../mutator_registration.rs");
    assert!(parent.contains("#[cfg(test)]\nmod symbol_globals;"));
    let fixture = oracle();
    assert_ne!(fixture[7], fixture[8], "GNU records both writer orders");
    assert_ne!(fixture[10], fixture[11], "GNU permits a lost update");
}
