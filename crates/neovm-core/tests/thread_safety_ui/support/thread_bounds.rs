//! Conditional methods test the same bounds as thread transfer/sharing, while
//! keeping compiler diagnostics in fixture/crate code rather than std sources.
#![allow(dead_code)]

use std::marker::PhantomData;

pub struct ThreadTransfer<T>(PhantomData<T>);

impl<T: Send> ThreadTransfer<T> {
    pub fn check() {}
}

impl<T: Send + 'static> ThreadTransfer<T> {
    pub fn transfer(value: T) {
        drop(std::thread::spawn(move || drop(value)));
    }
}

pub struct ThreadSharing<T>(PhantomData<T>);

impl<T: Sync> ThreadSharing<T> {
    pub fn check() {}
}
