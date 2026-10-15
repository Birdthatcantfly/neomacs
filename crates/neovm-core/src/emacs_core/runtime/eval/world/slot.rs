//! A Context field whose state GNU keeps global (round 3w, plan-3w.md).
//!
//! Every such field is reached through its [`WorldSlot`], the one access
//! path for that state. Step 1 holds the value inline, so the slot has the
//! value's layout and every access compiles as before; step 2 moves the
//! value into the World and leaves the slot as a pointer to it, padded to
//! the value's size so no other Context field moves.
//!
//! No `AsRef`/`AsMut`: they would shadow the value's own `as_ref`/`as_mut`
//! (`Option::as_ref`) at every call site.

use std::ops::{Deref, DerefMut};

/// A Context field of GNU-global state; see the module documentation.
#[repr(transparent)]
#[derive(Default)]
pub(crate) struct WorldSlot<T>(T);

impl<T> WorldSlot<T> {
    #[inline(always)]
    pub(crate) const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for WorldSlot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> Deref for WorldSlot<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for WorldSlot<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<'a, T> IntoIterator for &'a WorldSlot<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    #[inline(always)]
    fn into_iter(self) -> Self::IntoIter {
        (&self.0).into_iter()
    }
}

impl<'a, T> IntoIterator for &'a mut WorldSlot<T>
where
    &'a mut T: IntoIterator,
{
    type Item = <&'a mut T as IntoIterator>::Item;
    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;
    #[inline(always)]
    fn into_iter(self) -> Self::IntoIter {
        (&mut self.0).into_iter()
    }
}
