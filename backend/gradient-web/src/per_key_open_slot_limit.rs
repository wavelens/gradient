/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct OpenSlotLimitPerKey<K> {
    pub(crate) semaphores: Mutex<HashMap<K, Weak<Semaphore>>>,
    max_open_slots_per_key: usize,
}

impl<K: Eq + Hash> OpenSlotLimitPerKey<K> {
    pub(crate) fn new(max_open_slots_per_key: usize) -> Self {
        Self {
            semaphores: Mutex::new(HashMap::new()),
            max_open_slots_per_key,
        }
    }

    pub(crate) fn try_acquire_open_slot_for_key(&self, key: K) -> Option<OwnedSemaphorePermit> {
        let mut semaphores = self
            .semaphores
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let semaphore = match semaphores.get(&key).and_then(Weak::upgrade) {
            Some(semaphore) => semaphore,
            None => {
                semaphores.retain(|_, semaphore| semaphore.strong_count() > 0);
                let semaphore = Arc::new(Semaphore::new(self.max_open_slots_per_key));
                semaphores.insert(key, Arc::downgrade(&semaphore));
                semaphore
            }
        };
        semaphore.try_acquire_owned().ok()
    }
}
