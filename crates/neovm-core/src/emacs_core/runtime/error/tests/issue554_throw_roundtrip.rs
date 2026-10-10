//! GNU's chosen catch policy survives rooted loader/public error boundaries.
use super::{Flow, collect_in_flight_flow_gc_roots, flow_from_eval_error, map_flow};
use crate::emacs_core::{Context, Value, print::print_value};
use crate::tagged::SymbolMode;

#[test]
fn issue554_throw_policy_survives_real_load_and_flag_unbinding() {
    // GNU eval.c:1477 selects using EQ before eval.c:1449 unbinds;
    // lread.c:2338-2340 evaluates loaded forms in the active handler chain.
    let fixtures = tempfile::tempdir().expect("private load fixture directory");
    let file = fixtures.path().join("throw-policy.el");
    for (source, catch_tag, expected) in [
        (
            "(let ((symbols-with-pos-enabled t)) (throw (position-symbol 'issue554-load-tag 0) 7))",
            "'issue554-load-tag",
            "(7 nil)",
        ),
        (
            "(let ((symbols-with-pos-enabled t)) (throw 'issue554-load-tag 8))",
            "(position-symbol 'issue554-load-tag 9)",
            "(8 nil)",
        ),
        (
            "(let ((symbols-with-pos-enabled nil)) (throw (position-symbol 'issue554-load-tag 0) 7))",
            "'issue554-load-tag",
            "(no-catch nil)",
        ),
    ] {
        std::fs::write(
            &file,
            format!(";;; -*- lexical-binding: nil -*-\n{source}\n"),
        )
        .expect("write exclusively owned fixture");
        let mut ctx = Context::new();
        let path = print_value(&Value::string(file.to_str().expect("UTF-8 fixture path")));
        let form = format!(
            "(let ((symbols-with-pos-enabled nil))
               (list (condition-case err
                         (catch {catch_tag} (load {path} nil t t) 'fell-through)
                       (no-catch (car err)))
                     symbols-with-pos-enabled))"
        );
        let value = ctx.eval_str(&form).expect("load/catch policy fixture");
        assert_eq!(print_value(&value), expected, "{source}");
    }
}

#[test]
fn issue554_cloned_owned_throw_roundtrip_preserves_policy_and_roots() {
    for policy in [SymbolMode::Bare, SymbolMode::Positioned] {
        let mut ctx = Context::new();
        let bare_tag = Value::symbol("issue554-cloned-throw-tag");
        let tag = ctx
            .tagged_heap
            .alloc_symbol_with_pos(bare_tag, Value::fixnum(5));
        let payload = Value::vector(vec![Value::fixnum(7)]);
        let owned = map_flow(Flow::throw_with_symbols(tag, payload, policy));
        let cloned = owned.clone();
        drop(owned);
        // Current Lisp policy must not replace the throw's already selected
        // equality. Collection has only the cloned owned error's pin here.
        ctx.set_variable("symbols-with-pos-enabled", Value::NIL);
        let mut roots = Vec::new();
        collect_in_flight_flow_gc_roots(&mut roots, ctx.tagged_heap.identity());
        assert!(roots.iter().any(|root| root.bits() == tag.bits()));
        assert!(roots.iter().any(|root| root.bits() == payload.bits()));
        ctx.gc_collect_exact();
        let roundtrip = flow_from_eval_error(cloned);
        let thrown = roundtrip.as_throw().expect("owned error stays a throw");
        assert_eq!(thrown.symbol_mode(), policy);
        assert_eq!(
            thrown.matches_tag(bare_tag),
            policy == SymbolMode::Positioned
        );
        assert!(
            thrown.matches_tag(tag),
            "identical tags match with either policy"
        );
        ctx.gc_collect_exact();
        assert_eq!(thrown.value.as_vector_data().unwrap()[0], Value::fixnum(7));
        assert_eq!(thrown.tag.as_symbol_with_pos_sym(), Some(bare_tag));
        drop(roundtrip);
        let mut roots = Vec::new();
        collect_in_flight_flow_gc_roots(&mut roots, ctx.tagged_heap.identity());
        assert!(roots.iter().all(|root| root.bits() != tag.bits()));
        assert!(roots.iter().all(|root| root.bits() != payload.bits()));
    }
}
