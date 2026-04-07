/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use entity::organization::Model as MOrganization;
use gradient_core::sources::{FlakePrefetcher, PrefetchedFlake};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct PrefetchCall {
    pub repository: String,
    pub organization_id: uuid::Uuid,
}

/// In-memory `FlakePrefetcher` for tests. Records each call and always returns
/// `None` (i.e. behaves like an HTTPS repo where Nix fetches on demand).
#[derive(Debug, Default)]
pub struct FakeFlakePrefetcher {
    pub calls: Mutex<Vec<PrefetchCall>>,
}

impl FakeFlakePrefetcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<PrefetchCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl FlakePrefetcher for FakeFlakePrefetcher {
    async fn prefetch(
        &self,
        _crypt_secret_file: String,
        _serve_url: String,
        repository: String,
        organization: MOrganization,
    ) -> Result<Option<PrefetchedFlake>> {
        self.calls.lock().unwrap().push(PrefetchCall {
            repository,
            organization_id: organization.id,
        });
        Ok(None)
    }
}
