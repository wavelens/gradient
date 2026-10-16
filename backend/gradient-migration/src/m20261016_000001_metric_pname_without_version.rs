/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
UPDATE public.derivation_metric m
SET pname = coalesce(nullif(d.pname, ''), regexp_replace(d.name, '-[0-9][^-]*$', ''))
FROM public.derivation d
WHERE d.id = m.derivation
  AND m.pname IS DISTINCT FROM coalesce(nullif(d.pname, ''), regexp_replace(d.name, '-[0-9][^-]*$', ''));
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
