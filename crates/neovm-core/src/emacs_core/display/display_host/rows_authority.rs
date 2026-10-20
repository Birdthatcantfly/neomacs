//! Audited type authority for the native services of a numeric Rows query.
use super::DisplayHost;
use std::any::{Any, TypeId};

/// Type-level contract for an audited host, not a geometry or owner proof.
///
/// The authority can be copied into the existing frontend query closure and
/// passed explicitly; it requires no Context, engine or TLS policy field.
/// It matches standard Any's actual concrete TypeId, not a self-reported name
/// or trait-object vtable address. Unknown hosts cannot safely mint a token.
/// This immutable type contract may cross mutators; it owns no host or Lisp
/// state. Each consumer still validates its own installed Context and host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InertRowsHostAuthority {
    host_type: TypeId,
}

static_assertions::assert_impl_all!(InertRowsHostAuthority: Send, Sync, Copy, std::fmt::Debug);

impl InertRowsHostAuthority {
    /// Approve the concrete native host type T for a closed numeric Rows scope.
    ///
    /// # Safety
    /// Audit T's geometry/image/font services for EVERY instance and state,
    /// including future changes to its implementation. image_catalog and
    /// image_catalog_shared must always return None. Font lookup/open/coverage
    /// and metric services must never provide external geometry to this scope.
    /// These accessors must be inert native operations: no Lisp, callbacks,
    /// reentry, collection/safepoints, owner destruction, mutable external
    /// resources or other observable side effects. This is not an assertion
    /// about popup/clipboard methods, which the closed Rows producer excludes.
    /// The consumer must still exclude those other entry points, actual cold
    /// face realization, dynamic forms and every unproved display dependency.
    /// It must validate the ACTUAL installed host's TypeId before trusting
    /// this contract and recheck all owner/GC/source/freshness guards.
    /// A token alone permits no saved pointer access or query-cache admission.
    pub unsafe fn for_type<T: DisplayHost>() -> Self {
        Self {
            host_type: TypeId::of::<T>(),
        }
    }

    pub(crate) fn accepts(self, host: &dyn DisplayHost) -> bool {
        // Fully qualified standard blanket Any; no overrideable host getter.
        Any::type_id(host) == self.host_type
    }

    pub(crate) fn host_type(self) -> TypeId {
        self.host_type
    }
}
