//! Phase 0 step 2, round 3c: thread admission turns the owner's live `let`
//! entries into owner bindings, publishes what the outermost ones hide as
//! canonical values, and is one-time and sticky. Ports C4's admission tests
//! (`facade_binding_admission_test.rs:38, :210, :238`) and reconstruction
//! tests (`facade_bindings_test.rs:648, :775`) to the owner-slab model.

use super::super::owner_bindings::{Channel, UndoTarget};
use super::super::{NativeUnwindAction, SpecBinding};
use super::{AdmissionError, ThreadingMode};
use crate::emacs_core::eval::Context;
use crate::emacs_core::intern::{SymId, intern};
use crate::emacs_core::print::print_value;
use crate::emacs_core::value::Value;

fn green() -> Context {
    crate::test_utils::init_test_tracing();
    Context::new_with_threading(ThreadingMode::Green)
}

fn eval(ev: &mut Context, src: &str) -> String {
    match ev.eval_str(src) {
        Ok(value) => print_value(&value),
        Err(error) => format!("ERR {error:?}"),
    }
}

fn canonical(ev: &mut Context, id: SymId, channel: Channel) -> String {
    print_value(&ev.owner_canonical_value(id, channel))
}

fn owner_entries(ev: &Context) -> usize {
    ev.specpdl
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SpecBinding::NativeUnwind {
                    action: NativeUnwindAction::OwnerBinding(_)
                }
            )
        })
        .count()
}

fn legacy_lets(ev: &Context) -> usize {
    ev.specpdl
        .iter()
        .filter(|entry| entry.let_bound_symbol().is_some())
        .count()
}

#[test]
fn admission_needs_a_green_world_and_is_sticky() {
    let mut sim = Context::new();
    assert!(matches!(sim.admit_threads(), Err(AdmissionError::NotGreen)));
    assert!(!sim.threads_admitted());

    let mut ev = green();
    ev.admit_threads().expect("admission");
    assert!(ev.threads_admitted());
    assert!(matches!(
        ev.admit_threads(),
        Err(AdmissionError::AlreadyAdmitted)
    ));
    assert!(ev.threads_admitted());
}

#[test]
fn admission_reconstructs_the_bottom_saved_value_and_nested_bindings() {
    let mut ev = green();
    eval(&mut ev, "(defvar tad-bottom 1)");
    let id = intern("tad-bottom");
    ev.try_specbind(id, Value::fixnum(2)).expect("bind");
    let inner = ev.specpdl.len();
    ev.try_specbind(id, Value::fixnum(3)).expect("bind");
    ev.admit_threads().expect("admission");

    assert!(ev.obarray.thread_projection_of(id).is_some());
    assert_eq!(owner_entries(&ev), 2);
    assert_eq!(legacy_lets(&ev), 0);
    assert_eq!(eval(&mut ev, "tad-bottom"), "3");
    assert_eq!(canonical(&mut ev, id, Channel::Default), "1");
    let undo: Vec<_> = ev
        .owner
        .slab()
        .expect("the owner slab")
        .records_of(id)
        .map(|record| record.undo)
        .collect();
    assert!(matches!(
        undo[..],
        [UndoTarget::Inherited { suspended: None }, UndoTarget::Previous(outer)]
            if outer.get() == Value::fixnum(2)
    ));

    ev.unbind_to(inner);
    assert_eq!(eval(&mut ev, "tad-bottom"), "2");
    ev.unbind_to(0);
    assert_eq!(eval(&mut ev, "tad-bottom"), "1");
    assert_eq!(canonical(&mut ev, id, Channel::Default), "1");
}

#[test]
fn admission_publishes_the_first_saved_value_per_channel() {
    let mut ev = green();
    eval(
        &mut ev,
        "(progn (defvar tad-first 10) (defvar tad-auto 20)
                (make-variable-buffer-local 'tad-auto)
                (set-buffer (get-buffer-create \"tad-local\"))
                (setq tad-auto 30))",
    );
    let first = intern("tad-first");
    let auto = intern("tad-auto");
    let buffer = ev.buffers.current_buffer_id().expect("a current buffer");
    ev.try_specbind(first, Value::fixnum(11)).expect("bind");
    ev.try_specbind(first, Value::fixnum(12)).expect("bind");
    // A local exists in the current buffer: these are LetLocal bindings.
    ev.try_specbind(auto, Value::fixnum(31)).expect("bind");
    ev.try_specbind(auto, Value::fixnum(32)).expect("bind");
    ev.admit_threads().expect("admission");

    assert_eq!(owner_entries(&ev), 4);
    assert_eq!(canonical(&mut ev, first, Channel::Default), "10");
    assert_eq!(canonical(&mut ev, auto, Channel::BufferLocal(buffer)), "30");
    assert_eq!(canonical(&mut ev, auto, Channel::Default), "20");
    assert_eq!(eval(&mut ev, "(list tad-first tad-auto)"), "(12 32)");
    ev.unbind_to(0);
    assert_eq!(eval(&mut ev, "(list tad-first tad-auto)"), "(10 30)");
}

#[test]
fn a_capacity_refusal_leaves_the_owner_as_it_was() {
    let mut ev = green();
    eval(&mut ev, "(defvar tad-capacity 1)");
    let id = intern("tad-capacity");
    ev.try_specbind(id, Value::fixnum(2)).expect("bind");
    let before = ev.specpdl.len();
    let projections = ev.thread_projection_count_for_test();
    let inhibit = ev.gc_inhibit_depth;

    assert!(matches!(
        ev.admit_threads_refusing_capacity_for_test(),
        Err(AdmissionError::Capacity)
    ));
    assert!(!ev.threads_admitted());
    assert!(ev.owner.is_dormant(), "no slab was installed");
    assert_eq!(ev.gc_inhibit_depth, inhibit);
    assert_eq!(ev.specpdl.len(), before);
    assert_eq!(legacy_lets(&ev), 1);
    assert_eq!(owner_entries(&ev), 0);
    assert!(ev.obarray.thread_projection_of(id).is_none());
    assert_eq!(ev.thread_projection_count_for_test(), projections);
    assert_eq!(eval(&mut ev, "tad-capacity"), "2");
    ev.unbind_to(0);
    assert_eq!(eval(&mut ev, "tad-capacity"), "1");

    // The refusal left nothing behind: admission still succeeds.
    ev.admit_threads().expect("admission");
    assert!(ev.obarray.thread_projection_of(id).is_some());
}

#[test]
fn a_collection_right_after_admission_keeps_hidden_values() {
    let mut ev = green();
    eval(&mut ev, "(defvar tad-gc (cons 41 42))");
    let id = intern("tad-gc");
    ev.try_specbind(id, Value::NIL).expect("bind");
    ev.admit_threads().expect("admission");
    ev.gc_collect_exact();
    eval(&mut ev, "(dotimes (_ 2000) (make-list 10 'garbage))");
    ev.gc_collect_exact();
    assert_eq!(eval(&mut ev, "tad-gc"), "nil");
    ev.unbind_to(0);
    assert_eq!(eval(&mut ev, "(car tad-gc)"), "41");
}

#[test]
fn an_inhibition_overflow_refuses_and_keeps_the_depth() {
    let mut ev = green();
    ev.gc_inhibit_depth = usize::MAX;
    assert!(matches!(
        ev.admit_threads(),
        Err(AdmissionError::InhibitDepthOverflow)
    ));
    assert_eq!(ev.gc_inhibit_depth, usize::MAX);
    assert!(!ev.threads_admitted());
    ev.gc_inhibit_depth = 0;
}

#[test]
fn a_symbol_born_after_admission_starts_on_void_and_holds_values() {
    let mut ev = green();
    ev.admit_threads().expect("admission");
    let id = intern("tad-born-later");
    ev.obarray.ensure_symbol_id(id);
    let projection = ev.obarray.thread_projection_of(id).expect("born projected");
    assert!(projection.is_void());
    assert_eq!(eval(&mut ev, "(boundp 'tad-born-later)"), "nil");
    assert_eq!(
        eval(&mut ev, "(progn (setq tad-born-later 7) tad-born-later)"),
        "7"
    );
    assert!(
        !ev.obarray
            .thread_projection_of(id)
            .expect("still projected")
            .is_void(),
        "a write materializes a descriptor of its own"
    );
    assert_eq!(
        eval(
            &mut ev,
            "(list (let ((tad-born-later 8)) tad-born-later) tad-born-later)"
        ),
        "(8 7)"
    );
}

#[test]
fn a_void_variable_stays_on_void_until_written() {
    let mut ev = green();
    eval(
        &mut ev,
        "(progn (defvar tad-void 0) (makunbound 'tad-void))",
    );
    ev.admit_threads().expect("admission");
    let id = intern("tad-void");
    assert!(
        ev.obarray
            .thread_projection_of(id)
            .expect("projected")
            .is_void()
    );
    assert_eq!(eval(&mut ev, "(boundp 'tad-void)"), "nil");
    assert_eq!(
        eval(
            &mut ev,
            "(list (let ((tad-void 1)) tad-void) (boundp 'tad-void))"
        ),
        "(1 nil)"
    );
}

#[test]
fn a_killed_local_binding_is_captured() {
    let mut ev = green();
    eval(
        &mut ev,
        "(progn (defvar tad-killed 1) (make-variable-buffer-local 'tad-killed)
                (set-buffer (get-buffer-create \"tad-killed\"))
                (setq tad-killed 2))",
    );
    let id = intern("tad-killed");
    let buffer = ev.buffers.current_buffer_id().expect("a current buffer");
    ev.try_specbind(id, Value::fixnum(3)).expect("bind");
    eval(&mut ev, "(kill-local-variable 'tad-killed)");
    ev.admit_threads().expect("admission");

    let record = *ev
        .owner
        .slab()
        .expect("the owner slab")
        .records_of(id)
        .next()
        .expect("the binding became an owner record");
    assert_eq!(record.channel, Channel::BufferLocal(buffer));
    assert!(matches!(record.undo, UndoTarget::Captured(saved) if saved.get() == Value::fixnum(2)));
    assert_eq!(eval(&mut ev, "tad-killed"), "1");
    // Made again, the local takes back what the binding hid at its pop.
    eval(&mut ev, "(make-local-variable 'tad-killed)");
    ev.unbind_to(0);
    assert_eq!(eval(&mut ev, "tad-killed"), "2");
}

#[test]
fn a_per_buffer_slot_binding_stays_in_place() {
    let mut ev = green();
    eval(
        &mut ev,
        "(progn (set-buffer (get-buffer-create \"tad-slot\"))
                (set (make-local-variable 'fill-column) 33))",
    );
    let id = intern("fill-column");
    ev.try_specbind(id, Value::fixnum(44)).expect("bind");
    ev.try_specbind(intern("buffer-undo-list"), Value::T)
        .expect("bind");
    ev.admit_threads().expect("admission");

    assert_eq!(owner_entries(&ev), 0);
    assert!(matches!(
        ev.specpdl[..],
        [SpecBinding::LetLocal { .. }, SpecBinding::LetLocal { .. }]
    ));
    assert!(ev.obarray.thread_projection_of(id).is_some());
    assert!(
        ev.obarray
            .thread_projection_of(intern("buffer-undo-list"))
            .is_none()
    );
    assert_eq!(
        eval(&mut ev, "(list fill-column buffer-undo-list)"),
        "(44 t)"
    );
    ev.unbind_to(0);
    assert_eq!(eval(&mut ev, "fill-column"), "33");
}

#[test]
fn admission_moves_the_function_epoch_and_republishes_mirrors() {
    let mut ev = green();
    let id = intern("inhibit-quit");
    ev.try_specbind(id, Value::T).expect("bind");
    let epoch = ev.obarray.function_epoch();
    ev.admit_threads().expect("admission");
    assert_ne!(ev.obarray.function_epoch(), epoch);
    assert_eq!(ev.inhibit_quit, Value::T, "the owner's binding is its view");
    assert_eq!(canonical(&mut ev, id, Channel::Default), "nil");
    ev.unbind_to(0);
    assert_eq!(ev.inhibit_quit, Value::NIL);
}

#[test]
fn an_owner_binding_of_debug_on_next_call_takes_the_header() {
    let mut ev = green();
    let id = intern("debug-on-next-call");
    ev.try_specbind(id, Value::T).expect("bind");
    ev.admit_threads().expect("admission");
    assert!(ev.debug_on_next_call_is_armed());
    assert_eq!(canonical(&mut ev, id, Channel::Default), "nil");
    ev.unbind_to(0);
    assert!(!ev.debug_on_next_call_is_armed());
}
