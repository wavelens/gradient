/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Bit 0: the view bit on all keys. A key with a cache pin: view cache. Any other key: view project. Bit 1: read store only on a key with a cache pin. Any other key: manage project settings, so these keys never get this bit. Permissions for state-managed keys come from the declaration of server state.
pub(crate) const UP: &str = "UPDATE public.api SET permission = permission | 3 \
        WHERE cache IS NOT NULL AND NOT managed AND permission & 3 <> 3; \
    UPDATE public.api SET permission = permission | 1 \
        WHERE cache IS NULL AND NOT managed AND permission & 1 = 0;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
