/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_trait::async_trait;
use gradient_entity::user;

#[async_trait]
pub trait CacheCreationLimit: Send + Sync + std::fmt::Debug {
    async fn allows_another_cache(&self, user: &user::Model) -> anyhow::Result<bool>;
}

#[derive(Debug)]
pub struct NoCacheCreationLimit;

#[async_trait]
impl CacheCreationLimit for NoCacheCreationLimit {
    async fn allows_another_cache(&self, _user: &user::Model) -> anyhow::Result<bool> {
        Ok(true)
    }
}
