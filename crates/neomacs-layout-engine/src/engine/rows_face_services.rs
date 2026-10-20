//! Stack-only realization observation selected at the canonical Rows entry.
//! Ordinary frame coordination is a separate zero-state specialization.
use crate::neovm_bridge::{FaceResolver, FaceServices, ObservedFaceServices};
use std::cell::Cell;

pub(super) trait BoundFaceServices {
    type Services: FaceServices;
    fn services(&self) -> &Self::Services;
}

impl BoundFaceServices for &FaceResolver {
    type Services = FaceResolver;
    #[inline]
    fn services(&self) -> &FaceResolver {
        self
    }
}

impl BoundFaceServices for ObservedFaceServices<'_> {
    type Services = Self;
    #[inline]
    fn services(&self) -> &Self {
        self
    }
}

pub(super) trait FaceServiceFactory {
    type Bound<'a>: BoundFaceServices
    where
        Self: 'a;
    fn bind<'a>(&'a self, resolver: &'a FaceResolver) -> Self::Bound<'a>;

    /// Observe the actual accepted-query diagnostic drain before it is logged.
    /// The ordinary specialization has no state or conditional observer work.
    #[inline]
    fn note_invalid_face_references(&self, _names: &[String]) {}
}

pub(super) struct NoopFaceServiceFactory;
impl FaceServiceFactory for NoopFaceServiceFactory {
    type Bound<'a> = &'a FaceResolver;
    #[inline]
    fn bind<'a>(&'a self, resolver: &'a FaceResolver) -> &'a FaceResolver {
        resolver
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiagnosticEffects {
    Unseen,
    Clear,
    Observed,
}

pub(super) struct RowsFaceServiceFactory {
    cold_entry: Cell<bool>,
    diagnostic_effects: Cell<DiagnosticEffects>,
}

impl RowsFaceServiceFactory {
    pub(super) fn new() -> Self {
        Self {
            cold_entry: Cell::new(false),
            diagnostic_effects: Cell::new(DiagnosticEffects::Unseen),
        }
    }
    pub(super) fn cold_entry(&self) -> &Cell<bool> {
        &self.cold_entry
    }
    pub(super) fn diagnostic_effects(&self) -> DiagnosticEffects {
        self.diagnostic_effects.get()
    }
}

impl FaceServiceFactory for RowsFaceServiceFactory {
    type Bound<'a> = ObservedFaceServices<'a>;
    #[inline]
    fn bind<'a>(&'a self, resolver: &'a FaceResolver) -> ObservedFaceServices<'a> {
        ObservedFaceServices::new(resolver, &self.cold_entry)
    }
    #[inline]
    fn note_invalid_face_references(&self, names: &[String]) {
        if !names.is_empty() {
            self.diagnostic_effects.set(DiagnosticEffects::Observed);
            return;
        }
        self.diagnostic_effects
            .set(match self.diagnostic_effects.get() {
                DiagnosticEffects::Unseen | DiagnosticEffects::Clear => DiagnosticEffects::Clear,
                DiagnosticEffects::Observed => DiagnosticEffects::Observed,
            });
    }
}

static_assertions::assert_eq_size!(NoopFaceServiceFactory, ());
static_assertions::assert_not_impl_any!(RowsFaceServiceFactory: Sync);
