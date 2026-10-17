//! Sealed rooted inputs and immediate root installation for parallel conses.
//!
//! This bridge never installs a heap in TLS or borrows a whole TaggedHeap.
//! Its unique run borrow keeps the canonical run obligation active until the
//! initialized cell is registered in the existing shared-root table.

use super::SharedRoot;
use crate::tagged::gc::{HeapIdentity, RegistrationError, RunAccess};

/// Only this bridge can construct inputs. The references retain both source
/// objects through allocation and the initialized result's root installation.
pub(crate) struct RootedConsInputs<'roots> {
    car: &'roots SharedRoot,
    cdr: &'roots SharedRoot,
}

impl RootedConsInputs<'_> {
    pub(crate) fn heap_identity(&self) -> HeapIdentity {
        self.car.heap_identity()
    }

    /// Opaque copies only: the caller still needs matching live run/catalog
    /// authority before interpreting either word as a Lisp object.
    pub(crate) fn words(&self) -> (usize, usize) {
        (self.car.word(), self.cdr.word())
    }
}

/// Consuming initialized storage into raw root parts requires this seal.
/// Its private field and absent constructors keep that operation confined to
/// the immediate registration sequence below, even within the crate.
pub(crate) struct ConsRootSeal {
    _private: (),
}

static_assertions::assert_not_impl_any!(RootedConsInputs<'static>: Clone, Copy);
static_assertions::assert_not_impl_any!(ConsRootSeal: Clone, Copy);

impl SharedRoot {
    #[cfg(test)]
    pub(crate) fn cons_word_for_test(&self) -> usize {
        self.word()
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn cons_from_run(
        run: &mut RunAccess<'_, '_>,
        car: &Self,
        cdr: &Self,
    ) -> Result<Self, RegistrationError> {
        let heap = run.heap_identity();
        // No allocation, word extraction or mutation precedes validation of
        // BOTH operands. Numeric heap identity alone cannot mint the inputs.
        if car.heap_identity() != heap || cdr.heap_identity() != heap {
            return Err(RegistrationError::WrongHeap);
        }
        let inputs = RootedConsInputs { car, cdr };
        let initialized = run.allocate_rooted_cons(&inputs)?;
        // The nonforgeable initialized grant proves a completely initialized
        // cell from this run's catalog. Only this module can consume it into
        // parts. Immediately register those parts: no callback, safepoint,
        // heap/TLS lookup, or run-loan release may enter this interval.
        let (heap, value) = initialized.into_root_parts(ConsRootSeal { _private: () });
        Ok(Self::in_heap(heap, value))
    }
}
