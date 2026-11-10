/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.retired_cache_signing_key (
    id uuid NOT NULL PRIMARY KEY,
    cache uuid NOT NULL,
    public_key text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    retired_at timestamp without time zone NOT NULL,
    CONSTRAINT "fk-retired_cache_signing_key-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE
);

CREATE INDEX "idx-retired_cache_signing_key-cache" ON public.retired_cache_signing_key USING btree (cache, retired_at DESC);

ALTER TABLE public.cached_path_signature ADD COLUMN previous_signing_key_signature bytea;
"#;

const DOWN: &str = r#"
ALTER TABLE public.cached_path_signature DROP COLUMN previous_signing_key_signature;

DROP TABLE public.retired_cache_signing_key;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
