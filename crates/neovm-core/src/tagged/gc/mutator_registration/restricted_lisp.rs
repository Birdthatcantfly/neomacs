//! Test-only restricted Lisp overlap. Production has no call into this module,
//! and `#[cfg(test)]` keeps it out of every `NEOVM_THREADS` mode.
//!
//! Owners allocate only through `RunSlice::cons_into` inside `with_slice`.
//! Hot operands live in branded `SliceRoot` slots. `SharedRoot` is cold setup.
//! A program that uses anything outside the closed subset is rejected before
//! any owner is activated.
#![cfg(not(feature = "gc-memory-telemetry"))]

use super::{
    AllStopped, HeapIdentity, Mutator, MutatorPermit, OwnerRootSlot, ParallelHeapEra,
    RegistrationError, Registry, RendezvousError, RunAccess, RunSlice, SliceRoot,
};
use crate::emacs_core::eval::Context;
use crate::tagged::transport::SharedRoot;
use crate::tagged::value::{Fixnum, TaggedValue};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

const STEP_CAP: u32 = 10_000;
const SPIN_WAIT: Duration = Duration::from_secs(60);
const CHANNEL_WAIT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    ConstNil { dst: u16 },
    ConstT { dst: u16 },
    ConstFixnum { dst: u16, value: i64 },
    Move { dst: u16, src: u16 },
    Add { dst: u16, a: u16, b: u16 },
    Sub { dst: u16, a: u16, b: u16 },
    Lt { dst: u16, a: u16, b: u16 },
    Eq { dst: u16, a: u16, b: u16 },
    BranchIfNil { cond: u16, target: u16 },
    Jump { target: u16 },
    Cons { dst: u16, car: u16, cdr: u16 },
    Car { dst: u16, cell: u16 },
    Cdr { dst: u16, cell: u16 },
    SetCar { cell: u16, value: u16 },
    SetCdr { cell: u16, value: u16 },
    Safepoint,
}

/// Surface syntax the gate sees. Excluded variants never lower into [`Op`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Insn {
    Allowed(Op),
    SymbolValue,
    Obarray,
    Buffer,
    HashTable,
    Specbind,
    Jit,
    WorldManager,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Abs {
    Nil,
    T,
    Fix(i64),
    Cons,
    Top,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reject {
    Excluded(&'static str),
    RejectedType,
    Overflow,
    Backedge,
    Slot,
    Unbounded,
}

#[derive(Debug, PartialEq, Eq)]
enum ExecError {
    Registration(RegistrationError),
    Rendezvous(RendezvousError),
    ForeignCons,
    Type,
    Overflow,
    Backedge,
    Slot,
}

impl From<RegistrationError> for ExecError {
    fn from(error: RegistrationError) -> Self {
        Self::Registration(error)
    }
}

impl From<RendezvousError> for ExecError {
    fn from(error: RendezvousError) -> Self {
        Self::Rendezvous(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Go {
    Forced,
    Threshold,
}

#[derive(Debug)]
struct Snap {
    index: usize,
    cons_count: usize,
    safepoints: usize,
    bytes: usize,
    slots: Vec<usize>,
    conses: HashSet<usize>,
}

#[derive(Debug)]
struct EndState {
    index: usize,
    lines: Vec<String>,
    slots: Vec<usize>,
    conses: HashSet<usize>,
    cons_count: usize,
    safepoints: usize,
}

struct Machine {
    pc: usize,
    backedges: u32,
    cons_count: usize,
    safepoints: usize,
    owned: HashSet<usize>,
    entered: bool,
    allocated: bool,
}

impl Machine {
    fn new() -> Self {
        Self {
            pc: 0,
            backedges: 0,
            cons_count: 0,
            safepoints: 0,
            owned: HashSet::new(),
            entered: false,
            allocated: false,
        }
    }

    fn restart(&mut self) {
        self.pc = 0;
        self.backedges = 0;
    }
}

struct Overlap {
    abort: AtomicBool,
    entered: AtomicUsize,
    allocated: AtomicUsize,
    seen_enter: Vec<AtomicUsize>,
    seen_alloc: Vec<AtomicUsize>,
    progress: Vec<AtomicUsize>,
    n: usize,
    watch_alloc: bool,
}

impl Overlap {
    fn new(n: usize, watch_alloc: bool) -> Self {
        let mut seen_enter = Vec::with_capacity(n);
        let mut seen_alloc = Vec::with_capacity(n);
        let mut progress = Vec::with_capacity(n);
        for _ in 0..n {
            seen_enter.push(AtomicUsize::new(0));
            seen_alloc.push(AtomicUsize::new(0));
            progress.push(AtomicUsize::new(0));
        }
        Self {
            abort: AtomicBool::new(false),
            entered: AtomicUsize::new(0),
            allocated: AtomicUsize::new(0),
            seen_enter,
            seen_alloc,
            progress,
            n,
            watch_alloc,
        }
    }

    fn arrive_enter(&self, index: usize) {
        arrive(&self.entered, &self.seen_enter[index], self.n, &self.abort);
    }

    fn arrive_alloc(&self, index: usize) {
        arrive(
            &self.allocated,
            &self.seen_alloc[index],
            self.n,
            &self.abort,
        );
    }
}

fn arrive(flag: &AtomicUsize, seen: &AtomicUsize, n: usize, abort: &AtomicBool) {
    flag.fetch_add(1, Ordering::SeqCst);
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        if abort.load(Ordering::SeqCst) {
            panic!("overlap aborted");
        }
        let observed = flag.load(Ordering::SeqCst);
        if observed == n {
            // Every owner has arrived and is still inside this slice.
            seen.store(n, Ordering::SeqCst);
            return;
        }
        if Instant::now() >= deadline {
            abort.store(true, Ordering::SeqCst);
            panic!("overlap witness timed out at {observed}/{n}");
        }
        thread::yield_now();
    }
}

struct PanicFlag<'a> {
    registry: &'a Registry,
    abort: &'a AtomicBool,
}

impl Drop for PanicFlag<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.abort.store(true, Ordering::SeqCst);
            self.registry.poison();
        }
    }
}

fn recv<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(CHANNEL_WAIT)
        .unwrap_or_else(|error| panic!("owner channel closed: {error}"))
}

fn retry_rv<T>(mut op: impl FnMut() -> Result<T, RendezvousError>) -> T {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match op() {
            Ok(value) => return value,
            Err(RendezvousError::Busy) if Instant::now() < deadline => thread::yield_now(),
            Err(error) => panic!("rendezvous: {error:?}"),
        }
    }
}

fn retry_reg<T>(mut op: impl FnMut() -> Result<T, RegistrationError>) -> T {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match op() {
            Ok(value) => return value,
            Err(RegistrationError::Busy) if Instant::now() < deadline => thread::yield_now(),
            Err(error) => panic!("registration: {error:?}"),
        }
    }
}

fn must<T>(result: Result<T, ExecError>) -> T {
    result.unwrap_or_else(|error| panic!("restricted lisp: {error:?}"))
}

fn lower(insns: &[Insn]) -> Result<Vec<Op>, Reject> {
    let mut ops = Vec::with_capacity(insns.len());
    for insn in insns {
        ops.push(match *insn {
            Insn::Allowed(op) => op,
            Insn::SymbolValue => return Err(Reject::Excluded("symbol")),
            Insn::Obarray => return Err(Reject::Excluded("obarray")),
            Insn::Buffer => return Err(Reject::Excluded("buffer")),
            Insn::HashTable => return Err(Reject::Excluded("hash-table")),
            Insn::Specbind => return Err(Reject::Excluded("specbind")),
            Insn::Jit => return Err(Reject::Excluded("jit")),
            Insn::WorldManager => return Err(Reject::Excluded("world")),
        });
    }
    Ok(ops)
}

/// Count a backedge when the target is at or behind the branching instruction.
/// The `bound`-th backedge is still allowed.
fn edge(backedges: &mut u32, bound: u32, from: u16, target: u16) -> bool {
    if target <= from {
        *backedges = backedges.saturating_add(1);
        if *backedges > bound {
            return false;
        }
    }
    true
}

fn slot_index(slot: u16, len: usize) -> Result<usize, Reject> {
    let slot = usize::from(slot);
    if slot >= len {
        Err(Reject::Slot)
    } else {
        Ok(slot)
    }
}

fn target_index(target: u16, len: usize) -> Result<usize, Reject> {
    let target = usize::from(target);
    if target > len {
        Err(Reject::Slot)
    } else {
        Ok(target)
    }
}

fn prevalidate(ops: &[Op], nslots: usize, bound: u32) -> Result<(), Reject> {
    prevalidate_with(ops, nslots, bound, &[])
}

/// Concrete execution of the constant program. Joining the head's nil and cons
/// states to Top would reject the list's own `BranchIfNil`, so the validator
/// follows the branch it can see and counts backedges.
fn prevalidate_with(
    ops: &[Op],
    nslots: usize,
    bound: u32,
    initial: &[(u16, Abs)],
) -> Result<(), Reject> {
    let mut slots = vec![Abs::Nil; nslots];
    for &(index, kind) in initial {
        slots[slot_index(index, nslots)?] = kind;
    }
    let mut pc = 0usize;
    let mut backedges = 0u32;
    let mut steps = 0u32;
    while pc < ops.len() {
        steps += 1;
        if steps > STEP_CAP {
            return Err(Reject::Unbounded);
        }
        let from = u16::try_from(pc).map_err(|_| Reject::Slot)?;
        match ops[pc] {
            Op::ConstNil { dst } => {
                slots[slot_index(dst, nslots)?] = Abs::Nil;
                pc += 1;
            }
            Op::ConstT { dst } => {
                slots[slot_index(dst, nslots)?] = Abs::T;
                pc += 1;
            }
            Op::ConstFixnum { dst, value } => {
                if Fixnum::try_from(value).is_err() {
                    return Err(Reject::Overflow);
                }
                slots[slot_index(dst, nslots)?] = Abs::Fix(value);
                pc += 1;
            }
            Op::Move { dst, src } => {
                let value = slots[slot_index(src, nslots)?];
                slots[slot_index(dst, nslots)?] = value;
                pc += 1;
            }
            Op::Add { dst, a, b } => {
                let value = fix_binop(slots.as_slice(), nslots, a, b, true)?;
                let index = slot_index(dst, nslots)?;
                slots[index] = value;
                pc += 1;
            }
            Op::Sub { dst, a, b } => {
                let value = fix_binop(slots.as_slice(), nslots, a, b, false)?;
                let index = slot_index(dst, nslots)?;
                slots[index] = value;
                pc += 1;
            }
            Op::Lt { dst, a, b } => {
                let (x, y) = fix_pair(slots.as_slice(), nslots, a, b)?;
                let index = slot_index(dst, nslots)?;
                slots[index] = if x < y { Abs::T } else { Abs::Nil };
                pc += 1;
            }
            Op::Eq { dst, a, b } => {
                let (x, y) = fix_pair(slots.as_slice(), nslots, a, b)?;
                let index = slot_index(dst, nslots)?;
                slots[index] = if x == y { Abs::T } else { Abs::Nil };
                pc += 1;
            }
            Op::BranchIfNil { cond, target } => {
                let dest = target_index(target, ops.len())?;
                match slots[slot_index(cond, nslots)?] {
                    Abs::Nil => {
                        if !edge(&mut backedges, bound, from, target) {
                            return Err(Reject::Backedge);
                        }
                        pc = dest;
                    }
                    Abs::T | Abs::Fix(_) | Abs::Cons => pc += 1,
                    Abs::Top => return Err(Reject::RejectedType),
                }
            }
            Op::Jump { target } => {
                let dest = target_index(target, ops.len())?;
                if !edge(&mut backedges, bound, from, target) {
                    return Err(Reject::Backedge);
                }
                pc = dest;
            }
            Op::Cons { dst, car, cdr } => {
                let _ = slot_index(car, nslots)?;
                let _ = slot_index(cdr, nslots)?;
                slots[slot_index(dst, nslots)?] = Abs::Cons;
                pc += 1;
            }
            Op::Car { dst, cell } | Op::Cdr { dst, cell } => {
                if slots[slot_index(cell, nslots)?] != Abs::Cons {
                    return Err(Reject::RejectedType);
                }
                slots[slot_index(dst, nslots)?] = Abs::Top;
                pc += 1;
            }
            Op::SetCar { cell, value } | Op::SetCdr { cell, value } => {
                if slots[slot_index(cell, nslots)?] != Abs::Cons {
                    return Err(Reject::RejectedType);
                }
                let _ = slot_index(value, nslots)?;
                pc += 1;
            }
            Op::Safepoint => pc += 1,
        }
    }
    Ok(())
}

fn fix_pair(slots: &[Abs], nslots: usize, a: u16, b: u16) -> Result<(i64, i64), Reject> {
    match (slots[slot_index(a, nslots)?], slots[slot_index(b, nslots)?]) {
        (Abs::Fix(x), Abs::Fix(y)) => Ok((x, y)),
        _ => Err(Reject::RejectedType),
    }
}

fn fix_binop(slots: &[Abs], nslots: usize, a: u16, b: u16, add: bool) -> Result<Abs, Reject> {
    let (x, y) = fix_pair(slots, nslots, a, b)?;
    let value = if add {
        x.checked_add(y)
    } else {
        x.checked_sub(y)
    };
    let Some(value) = value else {
        return Err(Reject::Overflow);
    };
    if Fixnum::try_from(value).is_err() {
        return Err(Reject::Overflow);
    }
    Ok(Abs::Fix(value))
}

fn try_admit(
    insns: &[Insn],
    nslots: usize,
    bound: u32,
    initial: &[(u16, Abs)],
    started: &AtomicUsize,
) -> Result<Vec<Op>, Reject> {
    let ops = lower(insns)?;
    prevalidate_with(&ops, nslots, bound, initial)?;
    // Activation is this flag. It moves only after the gate accepts the program.
    started.fetch_add(1, Ordering::Relaxed);
    Ok(ops)
}

fn list_ops(base: i64) -> Vec<Op> {
    // 0 nil, 1 base, 2 k, 3 limit, 4 head, 5 tail, 6 cell, 7 one, 8 cond, 9 car, 10 scratch
    let mut ops = vec![
        Op::ConstNil { dst: 0 },
        Op::ConstFixnum {
            dst: 1,
            value: base,
        },
        Op::ConstFixnum { dst: 2, value: 0 },
        Op::ConstFixnum { dst: 3, value: 8 },
        Op::ConstNil { dst: 4 },
        Op::ConstNil { dst: 5 },
        Op::ConstFixnum { dst: 7, value: 1 },
    ];
    let loop_at = ops.len() as u16;
    ops.push(Op::Lt { dst: 8, a: 2, b: 3 });
    let branch_at = ops.len();
    ops.push(Op::BranchIfNil { cond: 8, target: 0 });
    ops.push(Op::Add { dst: 9, a: 1, b: 2 });
    ops.push(Op::ConstNil { dst: 10 });
    ops.push(Op::Cons {
        dst: 6,
        car: 9,
        cdr: 10,
    });
    let head_branch = ops.len();
    ops.push(Op::BranchIfNil { cond: 4, target: 0 });
    ops.push(Op::SetCdr { cell: 5, value: 6 });
    ops.push(Op::Move { dst: 5, src: 6 });
    let jump_join = ops.len();
    ops.push(Op::Jump { target: 0 });
    let init = ops.len() as u16;
    ops.push(Op::Move { dst: 4, src: 6 });
    ops.push(Op::Move { dst: 5, src: 6 });
    let join = ops.len() as u16;
    ops.push(Op::Add { dst: 2, a: 2, b: 7 });
    ops.push(Op::Safepoint);
    ops.push(Op::Jump { target: loop_at });
    let done = ops.len() as u16;
    ops.push(Op::ConstFixnum { dst: 9, value: 42 });
    ops.push(Op::ConstNil { dst: 10 });
    ops.push(Op::Cons {
        dst: 6,
        car: 9,
        cdr: 10,
    });
    ops.push(Op::SetCar { cell: 4, value: 6 });
    ops.push(Op::Safepoint);
    ops[branch_at] = Op::BranchIfNil {
        cond: 8,
        target: done,
    };
    ops[head_branch] = Op::BranchIfNil {
        cond: 4,
        target: init,
    };
    ops[jump_join] = Op::Jump { target: join };
    ops
}

fn sum_ops() -> Vec<Op> {
    // 0 n, 1 s, 2 one, 3 cond. Result stays in slot 1.
    let mut ops = vec![
        Op::ConstFixnum { dst: 0, value: 5 },
        Op::ConstFixnum { dst: 1, value: 0 },
        Op::ConstFixnum { dst: 2, value: 1 },
    ];
    let loop_at = ops.len() as u16;
    ops.push(Op::Lt { dst: 3, a: 0, b: 2 });
    let branch_at = ops.len();
    ops.push(Op::BranchIfNil { cond: 3, target: 0 });
    let jump_done = ops.len();
    ops.push(Op::Jump { target: 0 });
    let body = ops.len() as u16;
    ops.push(Op::Add { dst: 1, a: 1, b: 0 });
    ops.push(Op::Sub { dst: 0, a: 0, b: 2 });
    ops.push(Op::Safepoint);
    ops.push(Op::Jump { target: loop_at });
    let done = ops.len() as u16;
    ops[branch_at] = Op::BranchIfNil {
        cond: 3,
        target: body,
    };
    ops[jump_done] = Op::Jump { target: done };
    ops
}

fn if_ops(cond_nil: bool) -> Vec<Op> {
    let mut ops = vec![
        if cond_nil {
            Op::ConstNil { dst: 1 }
        } else {
            Op::ConstT { dst: 1 }
        },
        Op::ConstFixnum { dst: 2, value: 1 },
        Op::ConstFixnum { dst: 3, value: 2 },
    ];
    let branch = ops.len();
    ops.push(Op::BranchIfNil { cond: 1, target: 0 });
    ops.push(Op::Move { dst: 0, src: 2 });
    let jump = ops.len();
    ops.push(Op::Jump { target: 0 });
    let else_at = ops.len() as u16;
    ops.push(Op::Move { dst: 0, src: 3 });
    let end = ops.len() as u16;
    ops[branch] = Op::BranchIfNil {
        cond: 1,
        target: else_at,
    };
    ops[jump] = Op::Jump { target: end };
    ops
}

fn arithmetic_ops() -> Vec<Vec<Op>> {
    vec![
        vec![
            Op::ConstFixnum { dst: 1, value: 40 },
            Op::ConstFixnum { dst: 2, value: 2 },
            Op::Add { dst: 0, a: 1, b: 2 },
        ],
        vec![
            Op::ConstFixnum { dst: 1, value: 10 },
            Op::ConstFixnum { dst: 2, value: 3 },
            Op::Sub { dst: 0, a: 1, b: 2 },
        ],
        vec![
            Op::ConstFixnum { dst: 1, value: 1 },
            Op::ConstFixnum { dst: 2, value: 2 },
            Op::Lt { dst: 0, a: 1, b: 2 },
        ],
        vec![
            Op::ConstFixnum { dst: 1, value: 2 },
            Op::ConstFixnum { dst: 2, value: 1 },
            Op::Lt { dst: 0, a: 1, b: 2 },
        ],
        vec![
            Op::ConstFixnum { dst: 1, value: 3 },
            Op::ConstFixnum { dst: 2, value: 3 },
            Op::Eq { dst: 0, a: 1, b: 2 },
        ],
        vec![
            Op::ConstFixnum { dst: 1, value: 3 },
            Op::ConstFixnum { dst: 2, value: 4 },
            Op::Eq { dst: 0, a: 1, b: 2 },
        ],
        if_ops(true),
        if_ops(false),
    ]
}

fn list_stop(cons_count: usize, _safepoints: usize) -> Option<Go> {
    match cons_count {
        2 | 6 | 9 => Some(Go::Forced),
        4 | 8 => Some(Go::Threshold),
        _ => None,
    }
}

fn sum_stop(_cons_count: usize, safepoints: usize) -> Option<Go> {
    if safepoints == 1 {
        Some(Go::Forced)
    } else {
        None
    }
}

fn no_stop(_cons_count: usize, _safepoints: usize) -> Option<Go> {
    None
}

fn oracle_lines() -> Vec<&'static str> {
    let lines: Vec<_> = include_str!("restricted_lisp_oracle.expected")
        .lines()
        .collect();
    assert_eq!(lines.len(), 13, "GNU 31.1 oracle fixture");
    lines
}

fn prin1(bits: usize) -> String {
    fn walk(value: TaggedValue, out: &mut String, depth: usize) {
        assert!(depth < 32, "prin1 depth");
        if value.is_nil() {
            out.push_str("nil");
            return;
        }
        if value.is_t() {
            out.push_str("t");
            return;
        }
        if let Some(number) = value.as_fixnum() {
            out.push_str(&number.to_string());
            return;
        }
        assert!(value.is_cons(), "prin1 of {:#x}", value.bits());
        out.push('(');
        let mut cursor = value;
        let mut first = true;
        for _ in 0..32 {
            if !first {
                out.push(' ');
            }
            first = false;
            walk(cursor.cons_car_unobserved(), out, depth + 1);
            cursor = cursor.cons_cdr_unobserved();
            if cursor.is_nil() {
                out.push(')');
                return;
            }
            assert!(cursor.is_cons(), "dotted prin1");
        }
        panic!("prin1 length");
    }
    let mut out = String::new();
    walk(TaggedValue::from_bits(bits), &mut out, 0);
    out
}

fn home_index(slot: u16, len: usize) -> Result<usize, ExecError> {
    let slot = usize::from(slot);
    if slot >= len {
        Err(ExecError::Slot)
    } else {
        Ok(slot)
    }
}

fn jump_to(target: u16, len: usize) -> Result<usize, ExecError> {
    let target = usize::from(target);
    if target > len {
        Err(ExecError::Slot)
    } else {
        Ok(target)
    }
}

fn home_bits<'brand>(slice: &RunSlice<'brand, '_>, home: &SliceRoot<'brand>) -> usize {
    slice.word_bits_for_test(home)
}

fn exec<'brand>(
    slice: &mut RunSlice<'brand, '_>,
    homes: &mut [SliceRoot<'brand>],
    ops: &[Op],
    machine: &mut Machine,
    bound: u32,
    overlap: Option<&Overlap>,
    index: usize,
) -> Result<bool, ExecError> {
    if let Some(overlap) = overlap
        && !machine.entered
    {
        overlap.arrive_enter(index);
        machine.entered = true;
    }
    let mut steps = 0u32;
    while machine.pc < ops.len() {
        steps += 1;
        if steps > STEP_CAP {
            return Err(ExecError::Backedge);
        }
        let from = u16::try_from(machine.pc).map_err(|_| ExecError::Slot)?;
        match ops[machine.pc] {
            Op::ConstNil { dst } => {
                let dst = home_index(dst, homes.len())?;
                slice.store_immediate(&mut homes[dst], TaggedValue::NIL.bits())?;
                machine.pc += 1;
            }
            Op::ConstT { dst } => {
                let dst = home_index(dst, homes.len())?;
                slice.store_immediate(&mut homes[dst], TaggedValue::T.bits())?;
                machine.pc += 1;
            }
            Op::ConstFixnum { dst, value } => {
                let fix = Fixnum::try_from(value).map_err(|_| ExecError::Overflow)?;
                let dst = home_index(dst, homes.len())?;
                slice.store_immediate(&mut homes[dst], TaggedValue::from_fixnum(fix).bits())?;
                machine.pc += 1;
            }
            Op::Move { dst, src } => {
                let src = home_index(src, homes.len())?;
                let dst = home_index(dst, homes.len())?;
                let bits = home_bits(slice, &homes[src]);
                slice.plant_word_for_test(&mut homes[dst], bits);
                machine.pc += 1;
            }
            Op::Add { dst, a, b } => {
                store_fixnum(slice, homes, dst, fix_calc(slice, homes, a, b, true)?)?;
                machine.pc += 1;
            }
            Op::Sub { dst, a, b } => {
                store_fixnum(slice, homes, dst, fix_calc(slice, homes, a, b, false)?)?;
                machine.pc += 1;
            }
            Op::Lt { dst, a, b } => {
                let (x, y) = fix_operands(slice, homes, a, b)?;
                let bits = if x < y {
                    TaggedValue::T.bits()
                } else {
                    TaggedValue::NIL.bits()
                };
                let dst = home_index(dst, homes.len())?;
                slice.store_immediate(&mut homes[dst], bits)?;
                machine.pc += 1;
            }
            Op::Eq { dst, a, b } => {
                let (x, y) = fix_operands(slice, homes, a, b)?;
                let bits = if x == y {
                    TaggedValue::T.bits()
                } else {
                    TaggedValue::NIL.bits()
                };
                let dst = home_index(dst, homes.len())?;
                slice.store_immediate(&mut homes[dst], bits)?;
                machine.pc += 1;
            }
            Op::BranchIfNil { cond, target } => {
                let cond = home_index(cond, homes.len())?;
                let dest = jump_to(target, ops.len())?;
                if TaggedValue::from_bits(home_bits(slice, &homes[cond])).is_nil() {
                    if !edge(&mut machine.backedges, bound, from, target) {
                        return Err(ExecError::Backedge);
                    }
                    machine.pc = dest;
                } else {
                    machine.pc += 1;
                }
            }
            Op::Jump { target } => {
                let dest = jump_to(target, ops.len())?;
                if !edge(&mut machine.backedges, bound, from, target) {
                    return Err(ExecError::Backedge);
                }
                machine.pc = dest;
            }
            Op::Cons { dst, car, cdr } => {
                let car_i = home_index(car, homes.len())?;
                let cdr_i = home_index(cdr, homes.len())?;
                let dst_i = home_index(dst, homes.len())?;
                let car_home = homes[car_i].clone();
                let cdr_home = homes[cdr_i].clone();
                slice.cons_into(&car_home, &cdr_home, &mut homes[dst_i])?;
                let born = home_bits(slice, &homes[dst_i]);
                machine.owned.insert(born);
                machine.cons_count += 1;
                if let Some(overlap) = overlap {
                    overlap.progress[index].fetch_add(1, Ordering::SeqCst);
                    if overlap.watch_alloc && !machine.allocated {
                        overlap.arrive_alloc(index);
                        machine.allocated = true;
                    }
                }
                machine.pc += 1;
            }
            Op::Car { dst, cell } => {
                field_load(slice, homes, dst, cell, true)?;
                machine.pc += 1;
            }
            Op::Cdr { dst, cell } => {
                field_load(slice, homes, dst, cell, false)?;
                machine.pc += 1;
            }
            Op::SetCar { cell, value } => {
                field_store(slice, homes, &machine.owned, cell, value, true)?;
                machine.pc += 1;
            }
            Op::SetCdr { cell, value } => {
                field_store(slice, homes, &machine.owned, cell, value, false)?;
                machine.pc += 1;
            }
            Op::Safepoint => {
                machine.safepoints += 1;
                machine.pc += 1;
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn fix_operands<'brand>(
    slice: &RunSlice<'brand, '_>,
    homes: &[SliceRoot<'brand>],
    a: u16,
    b: u16,
) -> Result<(i64, i64), ExecError> {
    let a = home_index(a, homes.len())?;
    let b = home_index(b, homes.len())?;
    let x = TaggedValue::from_bits(home_bits(slice, &homes[a]))
        .as_fixnum()
        .ok_or(ExecError::Type)?;
    let y = TaggedValue::from_bits(home_bits(slice, &homes[b]))
        .as_fixnum()
        .ok_or(ExecError::Type)?;
    Ok((x, y))
}

fn fix_calc<'brand>(
    slice: &RunSlice<'brand, '_>,
    homes: &[SliceRoot<'brand>],
    a: u16,
    b: u16,
    add: bool,
) -> Result<Fixnum, ExecError> {
    let (x, y) = fix_operands(slice, homes, a, b)?;
    let value = if add {
        x.checked_add(y)
    } else {
        x.checked_sub(y)
    };
    let Some(value) = value else {
        return Err(ExecError::Overflow);
    };
    Fixnum::try_from(value).map_err(|_| ExecError::Overflow)
}

fn store_fixnum<'brand>(
    slice: &mut RunSlice<'brand, '_>,
    homes: &mut [SliceRoot<'brand>],
    dst: u16,
    value: Fixnum,
) -> Result<(), ExecError> {
    let dst = home_index(dst, homes.len())?;
    slice.store_immediate(&mut homes[dst], TaggedValue::from_fixnum(value).bits())?;
    Ok(())
}

fn field_load<'brand>(
    slice: &mut RunSlice<'brand, '_>,
    homes: &mut [SliceRoot<'brand>],
    dst: u16,
    cell: u16,
    car: bool,
) -> Result<(), ExecError> {
    let cell_i = home_index(cell, homes.len())?;
    let dst_i = home_index(dst, homes.len())?;
    let cell_home = homes[cell_i].clone();
    if !TaggedValue::from_bits(home_bits(slice, &cell_home)).is_cons() {
        return Err(ExecError::Type);
    }
    if car {
        slice.load_car(&cell_home, &mut homes[dst_i])?;
    } else {
        slice.load_cdr(&cell_home, &mut homes[dst_i])?;
    }
    Ok(())
}

fn field_store<'brand>(
    slice: &mut RunSlice<'brand, '_>,
    homes: &mut [SliceRoot<'brand>],
    owned: &HashSet<usize>,
    cell: u16,
    value: u16,
    car: bool,
) -> Result<(), ExecError> {
    let cell_i = home_index(cell, homes.len())?;
    let value_i = home_index(value, homes.len())?;
    let cell_bits = home_bits(slice, &homes[cell_i]);
    if !TaggedValue::from_bits(cell_bits).is_cons() {
        return Err(ExecError::Type);
    }
    if !owned.contains(&cell_bits) {
        return Err(ExecError::ForeignCons);
    }
    let value_home = homes[value_i].clone();
    if car {
        slice.store_car(&homes[cell_i], &value_home)?;
    } else {
        slice.store_cdr(&homes[cell_i], &value_home)?;
    }
    Ok(())
}

fn with_run<'era, R>(
    owner: &mut Mutator<'era>,
    body: impl FnOnce(&mut RunAccess<'_, 'era>) -> Result<R, ExecError>,
) -> Result<R, ExecError> {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match owner.run_access() {
            Ok(mut run) => {
                let result = body(&mut run);
                // `with_slice` already closed the region. Closing again is a no-op
                // and still runs when the body refused a foreign cons.
                if let Err(error) = run.close_cons_region() {
                    return Err(error.into());
                }
                return result;
            }
            Err(RegistrationError::Busy) if Instant::now() < deadline => thread::yield_now(),
            Err(error) => return Err(error.into()),
        }
    }
}

fn one_slice<'era>(
    owner: &mut Mutator<'era>,
    slots: &[OwnerRootSlot<'era>],
    ops: &[Op],
    machine: &mut Machine,
    bound: u32,
    overlap: Option<&Overlap>,
    index: usize,
) -> Result<(bool, usize, Vec<usize>), ExecError> {
    with_run(owner, |run| {
        let hit = run
            .with_slice(|slice| {
                let mut homes = Vec::with_capacity(slots.len());
                for slot in slots {
                    homes.push(slice.bind(slot)?);
                }
                exec(slice, &mut homes, ops, machine, bound, overlap, index)
            })
            .map_err(ExecError::from)??;
        let bytes = run.state().hot.bytes_since_gc;
        let words = slots.iter().map(|slot| slot.word_for_test(run)).collect();
        Ok((hit, bytes, words))
    })
}

fn activate_owner<'era>(
    mut permit: MutatorPermit<'era>,
    heap: HeapIdentity,
    id: u64,
) -> Mutator<'era> {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match permit.activate(heap, id) {
            Ok(owner) => return owner,
            Err(failure)
                if failure.reason == RegistrationError::Busy && Instant::now() < deadline =>
            {
                permit = failure.into_permit();
                thread::yield_now();
            }
            Err(failure) => panic!("activate: {:?}", failure.reason),
        }
    }
}

fn restore(owner: &mut Mutator<'_>) {
    let mut image = owner
        .begin_restore_retry(false)
        .unwrap_or_else(|error| panic!("restore: {error:?}"));
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match image.apply() {
            Ok(()) => return,
            Err(failure)
                if failure.reason == RendezvousError::Busy && Instant::now() < deadline =>
            {
                image = failure.into_image();
                thread::yield_now();
            }
            Err(failure) => panic!("apply: {:?}", failure.reason),
        }
    }
}

fn retire(mut owner: Mutator<'_>) {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match owner.retire_rooted(&[]) {
            Ok(()) => return,
            Err(failure)
                if failure.reason == RendezvousError::Busy && Instant::now() < deadline =>
            {
                owner = failure.into_owner();
                thread::yield_now();
            }
            Err(failure) => panic!("retire: {:?}", failure.reason),
        }
    }
}

fn complete(mut stopped: AllStopped<'_>) {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match stopped.complete() {
            Ok(()) => return,
            Err(failure)
                if failure.reason == RendezvousError::Busy && Instant::now() < deadline =>
            {
                stopped = failure.into_stopped();
                thread::yield_now();
            }
            Err(failure) => panic!("complete: {:?}", failure.reason),
        }
    }
}

fn on_heap<R>(
    era: &ParallelHeapEra<'_>,
    stopped: &AllStopped<'_>,
    body: impl Fn(&crate::tagged::gc::TaggedHeap) -> R,
) -> R {
    let deadline = Instant::now() + SPIN_WAIT;
    loop {
        match era.with_stopped_heap_for_test(stopped, &body) {
            Ok(value) => return value,
            Err(RendezvousError::Busy) if Instant::now() < deadline => thread::yield_now(),
            Err(error) => panic!("stopped heap: {error:?}"),
        }
    }
}

fn open_collection<'a>(
    era: &'a ParallelHeapEra<'_>,
    go_txs: &[Sender<Go>],
    kind: Go,
    next_epoch: &mut u64,
) -> AllStopped<'a> {
    // Owners are blocked on this signal, outside `with_slice`, before `stop`.
    for tx in go_txs {
        tx.send(kind)
            .unwrap_or_else(|_| panic!("go channel closed"));
    }
    let stopped = retry_rv(|| era.stop());
    let epoch = stopped.epoch().get();
    assert_eq!(epoch, *next_epoch, "collection epochs stay strictly +1");
    *next_epoch += 1;
    retry_reg(|| era.return_cons_storage(&stopped));
    stopped
}

fn collect_major(era: &ParallelHeapEra<'_>, stopped: &mut AllStopped<'_>) {
    let report = retry_rv(|| era.collect_stopped(stopped));
    assert!(report.major, "restricted overlap collects a major cycle");
}

fn release_cycle(era: &ParallelHeapEra<'_>, stopped: AllStopped<'_>, resume: bool) {
    if resume {
        retry_reg(|| era.resume_cons_storage(&stopped));
    }
    complete(stopped);
}

fn gather<T>(rx: &Receiver<T>, n: usize, index_of: impl Fn(&T) -> usize) -> Vec<T> {
    let mut slots: Vec<Option<T>> = (0..n).map(|_| None).collect();
    for _ in 0..n {
        let item = recv(rx);
        let index = index_of(&item);
        assert!(slots[index].is_none(), "duplicate owner message");
        slots[index] = Some(item);
    }
    slots
        .into_iter()
        .map(|item| item.expect("message"))
        .collect()
}

fn seed_nils<'era>(
    owner: &mut Mutator<'era>,
    nil: &SharedRoot,
    nslots: usize,
) -> Vec<OwnerRootSlot<'era>> {
    must(with_run(owner, |run| {
        let mut slots = Vec::with_capacity(nslots);
        for _ in 0..nslots {
            slots.push(run.root_slot(nil)?);
        }
        Ok(slots)
    }))
}

fn shutdown_owner(
    mut owner: Mutator<'_>,
    index: usize,
    ready_tx: &Sender<usize>,
    done_rx: &Receiver<()>,
    retired_tx: &Sender<usize>,
) {
    must(with_run(&mut owner, |run| {
        run.clear_root_slots()?;
        Ok(())
    }));
    retry_rv(|| owner.enter_native(&[]));
    ready_tx
        .send(index)
        .unwrap_or_else(|_| panic!("ready channel closed"));
    recv(done_rx);
    retire(owner);
    retired_tx
        .send(index)
        .unwrap_or_else(|_| panic!("retired channel closed"));
}

struct OwnerLaunch<'permit, 'env> {
    permit: MutatorPermit<'permit>,
    heap: HeapIdentity,
    id: u64,
    nil: &'env SharedRoot,
    programs: Vec<Vec<Op>>,
    nslots: usize,
    bound: u32,
    line_slot: usize,
    stop_at: fn(usize, usize) -> Option<Go>,
    overlap: Option<&'env Overlap>,
    index: usize,
    pause_tx: Sender<Snap>,
    go_rx: Receiver<Go>,
    end_tx: Sender<EndState>,
    ready_tx: Sender<usize>,
    done_rx: Receiver<()>,
    retired_tx: Sender<usize>,
    registry: &'permit Registry,
    abort: &'env AtomicBool,
}

impl OwnerLaunch<'_, '_> {
    fn run(self) {
        let Self {
            permit,
            heap,
            id,
            nil,
            programs,
            nslots,
            bound,
            line_slot,
            stop_at,
            overlap,
            index,
            pause_tx,
            go_rx,
            end_tx,
            ready_tx,
            done_rx,
            retired_tx,
            registry,
            abort,
        } = self;
        let _panic = PanicFlag { registry, abort };
        let mut owner = activate_owner(permit, heap, id);
        let slots = seed_nils(&mut owner, nil, nslots);
        let mut machine = Machine::new();
        let mut lines = Vec::with_capacity(programs.len());
        let mut final_slots = Vec::new();
        for program in &programs {
            machine.restart();
            final_slots = run_program(
                &mut owner,
                &slots,
                program,
                &mut machine,
                bound,
                overlap,
                index,
                &pause_tx,
                &go_rx,
                stop_at,
            );
            lines.push(prin1(final_slots[line_slot]));
        }
        end_tx
            .send(EndState {
                index,
                lines,
                slots: final_slots,
                conses: machine.owned,
                cons_count: machine.cons_count,
                safepoints: machine.safepoints,
            })
            .unwrap_or_else(|_| panic!("end channel closed"));
        shutdown_owner(owner, index, &ready_tx, &done_rx, &retired_tx);
    }
}

fn run_program<'era>(
    owner: &mut Mutator<'era>,
    slots: &[OwnerRootSlot<'era>],
    ops: &[Op],
    machine: &mut Machine,
    bound: u32,
    overlap: Option<&Overlap>,
    index: usize,
    pause_tx: &Sender<Snap>,
    go_rx: &Receiver<Go>,
    stop_at: fn(usize, usize) -> Option<Go>,
) -> Vec<usize> {
    loop {
        let (hit, bytes, words) =
            must(one_slice(owner, slots, ops, machine, bound, overlap, index));
        if !hit {
            return words;
        }
        let Some(kind) = stop_at(machine.cons_count, machine.safepoints) else {
            continue;
        };
        pause_tx
            .send(Snap {
                index,
                cons_count: machine.cons_count,
                safepoints: machine.safepoints,
                bytes,
                slots: words,
                conses: machine.owned.clone(),
            })
            .unwrap_or_else(|_| panic!("pause channel closed"));
        let go = recv(go_rx);
        assert_eq!(go, kind);
        if kind == Go::Threshold {
            assert!(bytes > 0, "threshold safepoint requires bytes_since_gc > 0");
            retry_rv(|| owner.request_collection());
        }
        let epoch = retry_rv(|| owner.wait_request());
        retry_rv(|| owner.acknowledge(epoch, &[]));
        // Dropped `RunAccess` above. Restore waits through Tracing.
        restore(owner);
    }
}

fn with_admitted(body: impl FnOnce(&ParallelHeapEra, bool, &SharedRoot, &SharedRoot)) {
    let mut context = Context::new();
    let generational = context.tagged_heap.generational_enabled();
    // SAFETY: nil and a fixnum are immediates of this Context's heap.
    let nil = unsafe { SharedRoot::new(&context.tagged_heap, TaggedValue::NIL) };
    let one = unsafe { SharedRoot::new(&context.tagged_heap, TaggedValue::fixnum(1)) };
    context
        .admit_owned_parallel(|projection| {
            {
                let era = projection.era();
                era.start_cons_storage()
                    .unwrap_or_else(|error| panic!("cons catalog: {error:?}"));
                body(era, generational, &nil, &one);
            }
            let (collector, keeper) = projection
                .finish()
                .unwrap_or_else(|error| panic!("finish: {error:?}"));
            (collector, keeper, ())
        })
        .unwrap_or_else(|error| panic!("admit: {error:?}"));
}

struct Session<'body, 'heap> {
    era: &'body ParallelHeapEra<'heap>,
    go_txs: Vec<Sender<Go>>,
    done_txs: Vec<Sender<()>>,
    pause_rx: Receiver<Snap>,
    end_rx: Receiver<EndState>,
    ready_rx: Receiver<usize>,
    retired_rx: Receiver<usize>,
    next_epoch: u64,
}

impl<'body, 'heap> Session<'body, 'heap> {
    fn launch<'scope, 'env>(
        era: &'body ParallelHeapEra<'heap>,
        nil: &'env SharedRoot,
        id_base: u64,
        programs: &[Vec<Op>],
        nslots: usize,
        bound: u32,
        line_slot: usize,
        stop_at: fn(usize, usize) -> Option<Go>,
        overlap: Option<&'env Overlap>,
        abort: &'env AtomicBool,
        scope: &'scope thread::Scope<'scope, 'env>,
    ) -> Self
    where
        'body: 'env,
    {
        let n = programs.len();
        let heap = era.heap_identity();
        let registry = era.registry();
        let (pause_tx, pause_rx) = channel();
        let (end_tx, end_rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let (retired_tx, retired_rx) = channel();
        let mut go_txs = Vec::with_capacity(n);
        let mut done_txs = Vec::with_capacity(n);
        // Design 3.3: fixed participants register while stopped, before any
        // owner thread exists. `OwnerLaunch::run` activates immediately, and
        // activation holds the registry gate; registering owner i+1 after
        // spawning owner i races that gate, which answers Busy instead of
        // blocking — a slow scheduler (TSan) turns the harness itself into
        // the failure. Register every permit first, then hand each to its
        // thread.
        let mut permits = Vec::with_capacity(n);
        for index in 0..n {
            let id = id_base + index as u64;
            permits.push(retry_reg(|| era.register(id, nslots)));
        }
        for (index, permit) in permits.into_iter().enumerate() {
            let id = id_base + index as u64;
            let (go_tx, go_rx) = channel();
            let (done_tx, done_rx) = channel();
            go_txs.push(go_tx);
            done_txs.push(done_tx);
            let launch = OwnerLaunch {
                permit,
                heap,
                id,
                nil,
                programs: vec![programs[index].clone()],
                nslots,
                bound,
                line_slot,
                stop_at,
                overlap,
                index,
                pause_tx: pause_tx.clone(),
                go_rx,
                end_tx: end_tx.clone(),
                ready_tx: ready_tx.clone(),
                done_rx,
                retired_tx: retired_tx.clone(),
                registry,
                abort,
            };
            scope.spawn(move || launch.run());
        }
        drop(pause_tx);
        drop(end_tx);
        drop(ready_tx);
        drop(retired_tx);
        Self {
            era,
            go_txs,
            done_txs,
            pause_rx,
            end_rx,
            ready_rx,
            retired_rx,
            next_epoch: 1,
        }
    }

    fn pauses(&self) -> Vec<Snap> {
        gather(&self.pause_rx, self.go_txs.len(), |snap| snap.index)
    }

    fn round(
        &mut self,
        kind: Go,
        resume: bool,
        after: impl FnOnce(&ParallelHeapEra<'heap>, &AllStopped<'_>),
    ) {
        let mut stopped = open_collection(self.era, &self.go_txs, kind, &mut self.next_epoch);
        collect_major(self.era, &mut stopped);
        after(self.era, &stopped);
        release_cycle(self.era, stopped, resume);
    }

    fn round_with_before(
        &mut self,
        kind: Go,
        resume: bool,
        before: impl FnOnce(&ParallelHeapEra<'heap>, &AllStopped<'_>),
        after: impl FnOnce(&ParallelHeapEra<'heap>, &AllStopped<'_>),
    ) {
        let mut stopped = open_collection(self.era, &self.go_txs, kind, &mut self.next_epoch);
        before(self.era, &stopped);
        collect_major(self.era, &mut stopped);
        after(self.era, &stopped);
        release_cycle(self.era, stopped, resume);
    }

    fn finish_owners(
        &mut self,
        after: impl FnOnce(&ParallelHeapEra<'heap>, &AllStopped<'_>),
    ) -> Vec<EndState> {
        let ends = gather(&self.end_rx, self.go_txs.len(), |end| end.index);
        let mut seen = vec![false; self.go_txs.len()];
        for _ in 0..self.go_txs.len() {
            let index = recv(&self.ready_rx);
            assert!(!seen[index], "duplicate ready");
            seen[index] = true;
        }
        let mut stopped = retry_rv(|| self.era.stop());
        let epoch = stopped.epoch().get();
        assert_eq!(epoch, self.next_epoch, "reclaim epoch");
        self.next_epoch += 1;
        retry_reg(|| self.era.return_cons_storage(&stopped));
        let report = retry_rv(|| self.era.collect_stopped(&mut stopped));
        assert!(report.major);
        after(self.era, &stopped);
        // Catalog stays on the heap: `finish` refuses a suspended cons catalog.
        complete(stopped);
        for tx in &self.done_txs {
            tx.send(())
                .unwrap_or_else(|_| panic!("done channel closed"));
        }
        for _ in 0..self.go_txs.len() {
            recv(&self.retired_rx);
        }
        ends
    }
}

fn dump_witness(overlap: &Overlap, label: &str) {
    let enter: Vec<_> = (0..overlap.n)
        .map(|index| overlap.seen_enter[index].load(Ordering::SeqCst))
        .collect();
    let alloc: Vec<_> = (0..overlap.n)
        .map(|index| overlap.seen_alloc[index].load(Ordering::SeqCst))
        .collect();
    let progress: Vec<_> = (0..overlap.n)
        .map(|index| overlap.progress[index].load(Ordering::SeqCst))
        .collect();
    println!(
        "A6_OVERLAP {label} owners={} seen_enter={enter:?} seen_alloc={alloc:?} progress={progress:?}",
        overlap.n
    );
}

fn assert_owned_conses(heap: &crate::tagged::gc::TaggedHeap, conses: &HashSet<usize>) {
    for bits in conses {
        let cell = TaggedValue::from_bits(*bits);
        assert!(cell.is_cons());
        assert!(
            heap.owns_heap_value_for_test(cell),
            "collection dropped a rooted cons {bits:#x}"
        );
    }
}

fn pass_gate(programs: &[Vec<Op>], nslots: usize, bound: u32, started: &AtomicUsize) {
    for program in programs {
        prevalidate(program, nslots, bound)
            .unwrap_or_else(|error| panic!("prevalidate: {error:?}"));
    }
    started.fetch_add(1, Ordering::Relaxed);
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

const LIST_ROUNDS: [(usize, Go); 5] = [
    (2, Go::Forced),
    (4, Go::Threshold),
    (6, Go::Forced),
    (8, Go::Threshold),
    (9, Go::Forced),
];

fn overlap_lists(owners: usize) {
    let started = AtomicUsize::new(0);
    let programs: Vec<_> = (0..owners)
        .map(|index| list_ops(index as i64 * 1_000_000))
        .collect();
    pass_gate(&programs, 11, 8, &started);
    let overlap = Overlap::new(owners, true);
    with_admitted(|era, generational, nil, _one| {
        thread::scope(|scope| {
            let _panic = PanicFlag {
                registry: era.registry(),
                abort: &overlap.abort,
            };
            let mut session = Session::launch(
                era,
                nil,
                6100,
                &programs,
                11,
                8,
                4,
                list_stop,
                Some(&overlap),
                &overlap.abort,
                scope,
            );
            let mut heads = vec![0usize; owners];
            let mut head_known = vec![false; owners];
            let mut phase9 = vec![HashSet::new(); owners];
            for (round, &(count, kind)) in LIST_ROUNDS.iter().enumerate() {
                let snaps = session.pauses();
                if round == 0 {
                    for index in 0..owners {
                        assert_eq!(overlap.seen_enter[index].load(Ordering::SeqCst), owners);
                        assert_eq!(overlap.seen_alloc[index].load(Ordering::SeqCst), owners);
                        assert!(overlap.progress[index].load(Ordering::SeqCst) >= 1);
                    }
                    dump_witness(&overlap, "first-stop");
                }
                for snap in &snaps {
                    assert_eq!(snap.cons_count, count);
                    assert_eq!(snap.conses.len(), count);
                    assert_eq!(overlap.progress[snap.index].load(Ordering::SeqCst), count);
                    if count < 8 {
                        assert!(snap.cons_count < 8, "unfinished rooted list");
                    }
                    let head = snap.slots[4];
                    assert!(TaggedValue::from_bits(head).is_cons());
                    if head_known[snap.index] {
                        assert_eq!(
                            heads[snap.index], head,
                            "head cons moved across a collection"
                        );
                    } else {
                        heads[snap.index] = head;
                        head_known[snap.index] = true;
                    }
                    if count == 9 {
                        let young = snap.slots[6];
                        assert_ne!(young, head, "old-to-young edge uses a fresh cons");
                        assert!(snap.conses.contains(&young));
                        assert!(TaggedValue::from_bits(young).is_cons());
                        phase9[snap.index] = snap.conses.clone();
                    }
                }
                assert_eq!(
                    heads.iter().collect::<HashSet<_>>().len(),
                    owners,
                    "owners built one head each"
                );
                let check = |heap: &crate::tagged::gc::TaggedHeap| {
                    for snap in &snaps {
                        assert_owned_conses(heap, &snap.conses);
                        assert!(
                            heap.owns_heap_value_for_test(TaggedValue::from_bits(snap.slots[4]))
                        );
                    }
                };
                if count == 9 {
                    session.round_with_before(
                        kind,
                        true,
                        |era, stopped| {
                            on_heap(era, stopped, |heap| {
                                for snap in &snaps {
                                    let head = TaggedValue::from_bits(snap.slots[4]);
                                    assert!(heap.owns_heap_value_for_test(head));
                                    if generational {
                                        assert!(
                                            heap.owner_card_is_dirty_for_test(head),
                                            "GEN1 setcar must dirty the old cons card"
                                        );
                                        let old = heap.cons_age_is_old_for_test(head);
                                        println!("A6_GEN1_HEAD_OLD={old}");
                                    }
                                }
                            });
                        },
                        |era, stopped| on_heap(era, stopped, check),
                    );
                } else {
                    session.round(kind, true, |era, stopped| on_heap(era, stopped, check));
                }
            }
            let ends = session.finish_owners(|era, stopped| {
                on_heap(era, stopped, |heap| {
                    let mut cells = 0usize;
                    for set in &phase9 {
                        for bits in set {
                            cells += 1;
                            assert!(
                                !heap.owns_heap_value_for_test(TaggedValue::from_bits(*bits)),
                                "uncleared cons still owned {bits:#x}"
                            );
                        }
                    }
                    println!("A6_RECLAIM owners={owners} cells={cells}");
                    assert_eq!(cells, owners * 9);
                });
            });
            dump_witness(&overlap, "done");
            let oracle = oracle_lines();
            for end in &ends {
                assert_eq!(end.cons_count, 9);
                assert_eq!(end.conses, phase9[end.index]);
                assert_eq!(end.slots[4], heads[end.index]);
                assert_eq!(end.lines.len(), 1);
                assert_eq!(end.lines[0], oracle[9 + end.index]);
                assert_eq!(overlap.progress[end.index].load(Ordering::SeqCst), 9);
            }
        });
    });
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

#[test]
fn a6_refuses_excluded_ops_before_run() {
    let started = AtomicUsize::new(0);
    let cases = [
        (Insn::SymbolValue, "symbol"),
        (Insn::Obarray, "obarray"),
        (Insn::Buffer, "buffer"),
        (Insn::HashTable, "hash-table"),
        (Insn::Specbind, "specbind"),
        (Insn::Jit, "jit"),
        (Insn::WorldManager, "world"),
    ];
    for (insn, name) in cases {
        let error = try_admit(&[insn], 1, 0, &[], &started).unwrap_err();
        assert_eq!(error, Reject::Excluded(name));
        assert_eq!(started.load(Ordering::Relaxed), 0);
    }
    let mixed = [Insn::Allowed(Op::ConstNil { dst: 0 }), Insn::SymbolValue];
    assert_eq!(
        try_admit(&mixed, 1, 0, &[], &started).unwrap_err(),
        Reject::Excluded("symbol")
    );
    assert_eq!(started.load(Ordering::Relaxed), 0);
}

#[test]
fn a6_refuses_overflow_and_unbounded_backedge_before_run() {
    let started = AtomicUsize::new(0);
    let overflow = [
        Insn::Allowed(Op::ConstFixnum {
            dst: 0,
            value: TaggedValue::MOST_POSITIVE_FIXNUM,
        }),
        Insn::Allowed(Op::ConstFixnum { dst: 1, value: 1 }),
        Insn::Allowed(Op::Add { dst: 2, a: 0, b: 1 }),
    ];
    assert_eq!(
        try_admit(&overflow, 3, 0, &[], &started).unwrap_err(),
        Reject::Overflow
    );
    let bare = [Insn::Allowed(Op::ConstFixnum {
        dst: 0,
        value: TaggedValue::MOST_POSITIVE_FIXNUM + 1,
    })];
    assert_eq!(
        try_admit(&bare, 1, 0, &[], &started).unwrap_err(),
        Reject::Overflow
    );
    let jump = [Insn::Allowed(Op::Jump { target: 0 })];
    assert_eq!(
        try_admit(&jump, 1, 0, &[], &started).unwrap_err(),
        Reject::Backedge
    );
    assert_eq!(
        try_admit(&jump, 1, u32::MAX, &[], &started).unwrap_err(),
        Reject::Unbounded
    );
    assert_eq!(started.load(Ordering::Relaxed), 0);
}

#[test]
fn a6_refuses_ambiguous_branch_before_run() {
    let started = AtomicUsize::new(0);
    let branch = [
        Insn::Allowed(Op::ConstNil { dst: 1 }),
        Insn::Allowed(Op::ConstNil { dst: 2 }),
        Insn::Allowed(Op::Cons {
            dst: 0,
            car: 1,
            cdr: 2,
        }),
        Insn::Allowed(Op::Car { dst: 3, cell: 0 }),
        Insn::Allowed(Op::BranchIfNil { cond: 3, target: 5 }),
    ];
    assert_eq!(
        try_admit(&branch, 4, 0, &[], &started).unwrap_err(),
        Reject::RejectedType
    );
    let bare = [Insn::Allowed(Op::SetCar { cell: 0, value: 1 })];
    assert_eq!(
        try_admit(&bare, 2, 0, &[], &started).unwrap_err(),
        Reject::RejectedType
    );
    assert_eq!(started.load(Ordering::Relaxed), 0);
}

#[test]
fn a6_runtime_refuses_foreign_cons_without_writing() {
    let started = AtomicUsize::new(0);
    let program = vec![
        Op::ConstFixnum { dst: 1, value: 9 },
        Op::SetCar { cell: 0, value: 1 },
    ];
    // Slot 0 is abstractly a cons, so the gate accepts the store. The runtime
    // owned-set is empty because this cons was not born from `cons_into`.
    prevalidate_with(&program, 2, 0, &[(0, Abs::Cons)])
        .unwrap_or_else(|error| panic!("prevalidate: {error:?}"));
    started.fetch_add(1, Ordering::Relaxed);
    assert_eq!(started.load(Ordering::Relaxed), 1);
    let abort = AtomicBool::new(false);
    with_admitted(|era, _generational, nil, one| {
        let (report_tx, report_rx) = channel();
        thread::scope(|scope| {
            let _panic = PanicFlag {
                registry: era.registry(),
                abort: &abort,
            };
            let heap = era.heap_identity();
            let registry = era.registry();
            let permit = era
                .register(6400, 2)
                .unwrap_or_else(|error| panic!("register: {error:?}"));
            let (ready_tx, ready_rx) = channel();
            let (done_tx, done_rx) = channel();
            let (retired_tx, retired_rx) = channel();
            let abort_flag = &abort;
            scope.spawn(move || {
                let _panic = PanicFlag {
                    registry,
                    abort: abort_flag,
                };
                let mut owner = activate_owner(permit, heap, 6400);
                let (foreign, slots) = must(with_run(&mut owner, |run| {
                    let foreign = run.cons(one, nil)?;
                    let head = run.root_slot(&foreign)?;
                    let scratch = run.root_slot(nil)?;
                    Ok((foreign, vec![head, scratch]))
                }));
                let mut machine = Machine::new();
                let outcome = one_slice(&mut owner, &slots, &program, &mut machine, 0, None, 0);
                let car =
                    TaggedValue::from_bits(foreign.cons_word_for_test()).cons_car_unobserved();
                report_tx
                    .send((outcome.err(), car.as_fixnum()))
                    .unwrap_or_else(|_| panic!("report channel closed"));
                drop(foreign);
                shutdown_owner(owner, 0, &ready_tx, &done_rx, &retired_tx);
            });
            let (error, car) = recv(&report_rx);
            assert_eq!(error, Some(ExecError::ForeignCons));
            assert_eq!(car, Some(1));
            assert_eq!(recv(&ready_rx), 0);
            let mut next_epoch = 1u64;
            let mut stopped = retry_rv(|| era.stop());
            assert_eq!(stopped.epoch().get(), next_epoch);
            next_epoch += 1;
            let _ = next_epoch;
            retry_reg(|| era.return_cons_storage(&stopped));
            let report = retry_rv(|| era.collect_stopped(&mut stopped));
            assert!(report.major);
            complete(stopped);
            done_tx.send(()).unwrap();
            assert_eq!(recv(&retired_rx), 0);
        });
    });
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

#[test]
fn a6_arithmetic_matches_gnu_oracle() {
    let started = AtomicUsize::new(0);
    let programs = arithmetic_ops();
    pass_gate(&programs, 4, 0, &started);
    let abort = AtomicBool::new(false);
    let oracle = oracle_lines();
    with_admitted(|era, _generational, nil, _one| {
        thread::scope(|scope| {
            let _panic = PanicFlag {
                registry: era.registry(),
                abort: &abort,
            };
            let heap = era.heap_identity();
            let registry = era.registry();
            let permit = era
                .register(6300, 4)
                .unwrap_or_else(|error| panic!("register: {error:?}"));
            let (end_tx, end_rx) = channel();
            let (ready_tx, ready_rx) = channel();
            let (done_tx, done_rx) = channel();
            let (retired_tx, retired_rx) = channel();
            let (pause_tx, _pause_rx) = channel();
            let (_go_tx, go_rx) = channel();
            let owned_programs = programs.clone();
            let abort_flag = &abort;
            scope.spawn(move || {
                OwnerLaunch {
                    permit,
                    heap,
                    id: 6300,
                    nil,
                    programs: owned_programs,
                    nslots: 4,
                    bound: 0,
                    line_slot: 0,
                    stop_at: no_stop,
                    overlap: None,
                    index: 0,
                    pause_tx,
                    go_rx,
                    end_tx,
                    ready_tx,
                    done_rx,
                    retired_tx,
                    registry,
                    abort: abort_flag,
                }
                .run()
            });
            let end = recv(&end_rx);
            assert_eq!(end.lines, oracle[..8]);
            assert_eq!(recv(&ready_rx), 0);
            let mut stopped = retry_rv(|| era.stop());
            assert_eq!(stopped.epoch().get(), 1);
            retry_reg(|| era.return_cons_storage(&stopped));
            assert!(retry_rv(|| era.collect_stopped(&mut stopped)).major);
            complete(stopped);
            done_tx.send(()).unwrap();
            assert_eq!(recv(&retired_rx), 0);
        });
    });
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

#[test]
fn a6_nonallocating_sum_loop() {
    let started = AtomicUsize::new(0);
    let programs = vec![sum_ops(), sum_ops()];
    pass_gate(&programs, 4, 5, &started);
    let overlap = Overlap::new(2, false);
    let oracle = oracle_lines();
    with_admitted(|era, _generational, nil, _one| {
        thread::scope(|scope| {
            let _panic = PanicFlag {
                registry: era.registry(),
                abort: &overlap.abort,
            };
            let mut session = Session::launch(
                era,
                nil,
                6200,
                &programs,
                4,
                5,
                1,
                sum_stop,
                Some(&overlap),
                &overlap.abort,
                scope,
            );
            let snaps = session.pauses();
            for index in 0..2 {
                assert_eq!(overlap.seen_enter[index].load(Ordering::SeqCst), 2);
                assert_eq!(overlap.seen_alloc[index].load(Ordering::SeqCst), 0);
            }
            dump_witness(&overlap, "sum-enter");
            for snap in &snaps {
                assert_eq!(snap.safepoints, 1);
                assert_eq!(snap.cons_count, 0);
                assert!(snap.conses.is_empty());
                assert_eq!(
                    TaggedValue::from_bits(snap.slots[0]).as_fixnum(),
                    Some(4),
                    "mid-loop n"
                );
                assert_eq!(
                    TaggedValue::from_bits(snap.slots[1]).as_fixnum(),
                    Some(5),
                    "mid-loop s"
                );
            }
            session.round(Go::Forced, true, |_era, _stopped| {});
            let ends = session.finish_owners(|_era, _stopped| {});
            for end in &ends {
                assert_eq!(end.safepoints, 5);
                assert_eq!(end.cons_count, 0);
                assert_eq!(end.lines, vec![oracle[8]]);
            }
        });
    });
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

#[test]
fn a6_overlap_lists_two_owners() {
    overlap_lists(2);
}

#[test]
fn a6_overlap_lists_four_owners() {
    overlap_lists(4);
}
