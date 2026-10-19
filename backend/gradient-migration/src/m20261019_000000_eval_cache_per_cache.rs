/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
DELETE FROM public.eval_cache_store;

ALTER TABLE public.eval_cache_store
    ADD COLUMN cache uuid NOT NULL REFERENCES public.cache(id) ON DELETE CASCADE;

DROP INDEX public."idx-eval_cache_store-fingerprint";

CREATE UNIQUE INDEX "idx-eval_cache_store-cache-fingerprint" ON public.eval_cache_store (cache, fingerprint);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "m20261019_000000_eval_cache_per_cache is irreversible".into(),
        ))
    }
}
