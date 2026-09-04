/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared-memory primitives whose failure modes we have designed away.

use std::fmt;
use std::sync::MutexGuard;

/// A mutex that ignores poisoning. Everything guarded this way is a plain
/// collection, queue or counter whose invariants survive a panic mid-critical
/// section, so a poisoned lock hands the value back instead of turning one
/// panic into a cascade of them at every later `lock()`.
pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(|e| e.into_inner())
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for Mutex<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Mutex").field(&&*self.lock()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Mutex;
    use std::sync::Arc;

    /// A panic while the lock is held must not turn every later `lock()` into
    /// a second panic: the guarded value is still there to be read.
    #[test]
    fn a_poisoned_lock_still_hands_out_the_value() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));
        let poisoner = Arc::clone(&m);
        let panicked = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("holding the lock");
        })
        .join();

        assert!(panicked.is_err(), "the helper thread must have panicked");
        assert_eq!(*m.lock(), vec![1, 2, 3]);
    }

    /// `into_inner` is the other half of the same promise: draining an
    /// accumulator after a worker panicked must still yield what it collected.
    #[test]
    fn into_inner_survives_a_poisoned_lock() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));
        let poisoner = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("holding the lock");
        })
        .join();

        let inner = Arc::into_inner(m).expect("the poisoner thread is joined");
        assert_eq!(inner.into_inner(), vec![1, 2, 3]);
    }

    #[test]
    fn the_guard_writes_through() {
        let m = Mutex::new(0);
        *m.lock() += 5;
        assert_eq!(*m.lock(), 5);
    }
}
