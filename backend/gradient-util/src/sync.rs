/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::sync::MutexGuard;

/// Guarded values are plain collections, queues or counters whose invariants survive a panic. A
/// poisoned lock is handing the value back instead of cascading one panic into every later
/// `lock()`.
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
}
