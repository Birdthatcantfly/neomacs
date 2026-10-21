//! Immutable admission selection for one engine-local Rows query.
//!
//! | Native startup option | Default | Meaning |
//! | --- | --- | --- |
//! | `--rows-query-admission=off\|exact-proven` | `exact-proven` | Request only privately certified original-scope Rows hits. |
//!
//! The existing boxed frontend query adapter owns this native configuration.
//! No engine, Context, runtime or TLS field stores it. Independent mutators
//! may copy the configuration; each query must prove its own live authorities.
use neovm_core::emacs_core::display_host::InertRowsHostAuthority;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, strum::EnumString)]
pub enum RowsQueryAdmission {
    #[strum(serialize = "off")]
    Off,
    #[default]
    #[strum(serialize = "exact-proven")]
    ExactProven,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum RowsQueryEnvironment {
    Graphical,
    Terminal(InertRowsHostAuthority),
}

/// A request to the private admission constructor, never proof of eligibility.
/// Values contain no Lisp state, host object, source owner or cached geometry.
#[derive(Clone, Copy, Debug)]
pub struct RowsQueryPolicy {
    admission: RowsQueryAdmission,
    environment: RowsQueryEnvironment,
}

impl Default for RowsQueryPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

impl RowsQueryPolicy {
    pub const fn disabled() -> Self {
        Self::graphical(RowsQueryAdmission::Off)
    }

    pub const fn graphical(admission: RowsQueryAdmission) -> Self {
        Self {
            admission,
            environment: RowsQueryEnvironment::Graphical,
        }
    }

    pub const fn terminal(admission: RowsQueryAdmission, host: InertRowsHostAuthority) -> Self {
        Self {
            admission,
            environment: RowsQueryEnvironment::Terminal(host),
        }
    }

    pub const fn admission(self) -> RowsQueryAdmission {
        self.admission
    }

    pub(super) const fn environment(self) -> RowsQueryEnvironment {
        self.environment
    }
}

static_assertions::assert_impl_all!(RowsQueryPolicy: Send, Sync, Copy, std::fmt::Debug);
