//! Cold predicate-gate ownership, including event-driven terminal poison.
use super::*;
use std::ops::{Deref, DerefMut};

static NEXT_GATE_THREAD: AtomicU64 = AtomicU64::new(1);
thread_local! {
    // No destructor or recycled OS identifier: zero always means unowned.
    static GATE_THREAD: u64 = issue_generation(&NEXT_GATE_THREAD)
        .expect("registry gate thread identity exhausted").get();
}
fn current_thread() -> u64 {
    GATE_THREAD.with(|thread| *thread)
}

/// The owner tag describes only a held real mutex, never substitutes for it.
/// Clearing precedes unlock; reacquisition restores it before inspecting state.
pub(super) struct RegistryGuard<'a> {
    registry: &'a Registry,
    guard: Option<MutexGuard<'a, RegistryState>>,
}
impl<'a> RegistryGuard<'a> {
    fn new(registry: &'a Registry, guard: MutexGuard<'a, RegistryState>) -> Self {
        registry
            .gate_owner
            .store(current_thread(), Ordering::Release);
        Self {
            registry,
            guard: Some(guard),
        }
    }
    fn take_for_wait(mut self) -> MutexGuard<'a, RegistryState> {
        self.registry.gate_owner.store(0, Ordering::Release);
        self.guard.take().expect("live registry guard")
    }
}
impl Deref for RegistryGuard<'_> {
    type Target = RegistryState;
    fn deref(&self) -> &Self::Target {
        self.guard.as_deref().expect("live registry guard")
    }
}
impl DerefMut for RegistryGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.as_deref_mut().expect("live registry guard")
    }
}
impl Drop for RegistryGuard<'_> {
    fn drop(&mut self) {
        if self.guard.is_some() {
            self.registry.gate_owner.store(0, Ordering::Release);
        }
    }
}

impl Registry {
    /// Internal stopped image prepare/commit only. Never wait while retaining a gate,
    /// entry/root-table/allocator guard, or a callback into owner code.
    pub(super) fn lock_blocking(&self) -> Result<RegistryGuard<'_>, RegistrationError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        if self.gate_owner.load(Ordering::Acquire) == current_thread() {
            return Err(RegistrationError::Busy);
        }
        match self.state.lock() {
            Ok(guard) => {
                let guard = RegistryGuard::new(self, guard);
                if self.poisoned.load(Ordering::Acquire) {
                    Err(RegistrationError::Poisoned)
                } else {
                    Ok(guard)
                }
            }
            Err(error) => {
                let _guard = RegistryGuard::new(self, error.into_inner());
                self.poison();
                Err(RegistrationError::Poisoned)
            }
        }
    }

    pub(super) fn lock(&self) -> Result<RegistryGuard<'_>, RegistrationError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        match self.state.try_lock() {
            Ok(guard) => {
                let guard = RegistryGuard::new(self, guard);
                if self.poisoned.load(Ordering::Acquire) {
                    Err(RegistrationError::Poisoned)
                } else {
                    Ok(guard)
                }
            }
            Err(std::sync::TryLockError::WouldBlock) => Err(RegistrationError::Busy),
            Err(std::sync::TryLockError::Poisoned(error)) => {
                // Install ownership BEFORE poison, avoiding recursive locking
                // while a recovered PoisonError still owns the real mutex.
                let _guard = RegistryGuard::new(self, error.into_inner());
                self.poison();
                Err(RegistrationError::Poisoned)
            }
        }
    }

    pub(super) fn poison(&self) {
        if self.gate_owner.load(Ordering::Acquire) == current_thread() {
            self.publish_poison();
        } else {
            // No publication, collector callback or join holds the control
            // gate. Entry locks use try_lock, so they cannot reverse this edge.
            let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let _guard = RegistryGuard::new(self, guard);
            self.publish_poison();
        }
    }
    fn publish_poison(&self) {
        self.poisoned.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    pub(super) fn wait<'a>(
        &'a self,
        guard: RegistryGuard<'a>,
    ) -> Result<RegistryGuard<'a>, RegistrationError> {
        debug_assert!(std::ptr::eq(self, guard.registry));
        // Includes same-thread poison after the caller's predicate check. An
        // external poisoner must obtain the mutex, so it cannot notify in the
        // following check->atomic-release/wait gap.
        if self.poisoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        match self.wake.wait(guard.take_for_wait()) {
            Ok(guard) => {
                let guard = RegistryGuard::new(self, guard);
                if self.poisoned.load(Ordering::Acquire) {
                    Err(RegistrationError::Poisoned)
                } else {
                    Ok(guard)
                }
            }
            Err(error) => {
                let _guard = RegistryGuard::new(self, error.into_inner());
                self.poison();
                Err(RegistrationError::Poisoned)
            }
        }
    }
}
