//! D-Bus transport matching GNU `src/dbusbind.c`.
//!
//! GNU compiles that file only under `#ifdef HAVE_DBUS`, set when
//! `configure.ac` finds `dbus-1 >= 1.0`. This module is the same switch:
//! `cfg(neomacs_have_dbus)` from `neovm-core/build.rs` (Unix). Without it the
//! subsystem still exists so startup can register nothing — GNU's
//! `--without-dbus` answers, which ledger 192 pinned.
//!
//! Lisp (`lisp/net/dbus.el`) stays GNU's. This crate owns the six primitives,
//! the nine `DEFVAR`s, `dbus-error`, `dbus-event`, and the wait-loop watches.

use crate::emacs_core::error::Flow;
use crate::emacs_core::eval::Context;

mod fd;
mod subrs;

std::cfg_select! {
    neomacs_have_dbus => {
        mod connection;
        mod event;
        mod message;
        mod types;
        mod watch;
    }
    _ => {}
}

std::cfg_select! {
    all(test, neomacs_have_dbus) => {
        pub(crate) use subrs::SUBRS;
    }
    _ => {}
}

/// D-Bus's carried World state (connections and registered fds; see
/// `connection::take_world_state`).
#[cold]
pub(crate) fn take_world_state() -> Box<dyn std::any::Any> {
    std::cfg_select! {
        neomacs_have_dbus => {
            Box::new((connection::take_world_state(), fd::take_world_state()))
        }
        _ => {
            Box::new(((), fd::take_world_state()))
        }
    }
}

#[cold]
pub(crate) fn restore_world_state(state: Box<dyn std::any::Any>) {
    std::cfg_select! {
        neomacs_have_dbus => {
            let (buses, fds) = *state
                .downcast::<(Box<dyn std::any::Any>, Box<dyn std::any::Any>)>()
                .expect("D-Bus's carried state");
            connection::restore_world_state(buses);
            fd::restore_world_state(fds);
        }
        _ => {
            let ((), fds) = *state
                .downcast::<((), Box<dyn std::any::Any>)>()
                .expect("D-Bus's carried state");
            fd::restore_world_state(fds);
        }
    }
}

pub(crate) fn register_subrs(ctx: &mut Context) {
    subrs::register_subrs(ctx);
}

/// GNU `syms_of_dbusbind`'s variables (see `subrs::register_bootstrap_vars`);
/// nothing in a build without libdbus.
pub(crate) fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    std::cfg_select! {
        neomacs_have_dbus => subrs::register_bootstrap_vars(obarray),
        _ => {
            let _ = obarray;
        }
    }
}

pub(crate) fn drain_events(ctx: &mut Context) -> Result<usize, Flow> {
    std::cfg_select! {
        neomacs_have_dbus => event::drain(ctx),
        _ => {
            let _ = ctx;
            Ok(0)
        }
    }
}

pub(crate) fn has_active_watches() -> bool {
    std::cfg_select! {
        neomacs_have_dbus => connection::has_buses(),
        _ => false,
    }
}

pub(crate) fn reset_thread_locals() {
    std::cfg_select! {
        neomacs_have_dbus => connection::reset(),
        _ => {}
    }
    fd::reset();
}

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
