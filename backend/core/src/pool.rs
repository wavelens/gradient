/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::executer::get_local_store;
use super::types::LocalNixStore;

/// Connection pool for local Nix daemon connections (Unix socket / subprocess).
///
/// Limits the number of simultaneous open connections via a semaphore.
/// Idle connections are reused to avoid reconnect overhead.
pub struct NixStorePool {
    idle: Arc<Mutex<Vec<LocalNixStore>>>,
    semaphore: Arc<Semaphore>,
}

impl std::fmt::Debug for NixStorePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NixStorePool")
            .field("available_permits", &self.semaphore.available_permits())
            .finish()
    }
}

/// A checked-out connection from `NixStorePool`.
///
/// Implements `DerefMut<Target = LocalNixStore>` so callers can use it
/// as a `&mut LocalNixStore`. Returns the connection to the pool on drop.
pub struct PooledStore {
    store: Option<LocalNixStore>,
    idle: Arc<Mutex<Vec<LocalNixStore>>>,
    _permit: OwnedSemaphorePermit,
}

impl NixStorePool {
    pub fn new(max: usize) -> Self {
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
            semaphore: Arc::new(Semaphore::new(max)),
        }
    }

    /// Acquire a connection, blocking until one is available.
    ///
    /// Returns an idle connection if one exists, otherwise opens a new one.
    pub async fn acquire(&self) -> Result<PooledStore> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("NixStorePool semaphore closed"))?;

        let store = self.idle.lock().unwrap().pop();

        let store = match store {
            Some(s) => s,
            None => get_local_store(None).await?,
        };

        Ok(PooledStore {
            store: Some(store),
            idle: Arc::clone(&self.idle),
            _permit: permit,
        })
    }
}

impl Deref for PooledStore {
    type Target = LocalNixStore;

    fn deref(&self) -> &Self::Target {
        self.store.as_ref().unwrap()
    }
}

impl DerefMut for PooledStore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.store.as_mut().unwrap()
    }
}

impl Drop for PooledStore {
    fn drop(&mut self) {
        if let Some(store) = self.store.take()
            && let Ok(mut idle) = self.idle.lock()
        {
            idle.push(store);
        }
    }
}
