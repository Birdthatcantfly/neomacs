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
}

pub(super) struct NoopFaceServiceFactory;
impl FaceServiceFactory for NoopFaceServiceFactory {
    type Bound<'a> = &'a FaceResolver;
    #[inline]
    fn bind<'a>(&'a self, resolver: &'a FaceResolver) -> &'a FaceResolver {
        resolver
    }
}

pub(super) struct RowsFaceServiceFactory {
    cold_entry: Cell<bool>,
}

impl RowsFaceServiceFactory {
    pub(super) fn new() -> Self {
        Self {
            cold_entry: Cell::new(false),
        }
    }
    pub(super) fn cold_entry(&self) -> &Cell<bool> {
        &self.cold_entry
    }
}

impl FaceServiceFactory for RowsFaceServiceFactory {
    type Bound<'a> = ObservedFaceServices<'a>;
    #[inline]
    fn bind<'a>(&'a self, resolver: &'a FaceResolver) -> ObservedFaceServices<'a> {
        ObservedFaceServices::new(resolver, &self.cold_entry)
    }
}

static_assertions::assert_eq_size!(NoopFaceServiceFactory, ());
static_assertions::assert_not_impl_any!(RowsFaceServiceFactory: Sync);
