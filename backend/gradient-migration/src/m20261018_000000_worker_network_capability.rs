/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.worker_registration
    ADD COLUMN enable_network boolean DEFAULT true NOT NULL,
    ADD COLUMN enable_secrets boolean DEFAULT true NOT NULL,
    ADD COLUMN enable_exec boolean DEFAULT false NOT NULL;

UPDATE public.worker_registration SET enable_network = enable_fetch, enable_secrets = enable_fetch;

ALTER TABLE public.worker_registration DROP COLUMN enable_fetch;

ALTER TABLE public.team_worker
    ADD COLUMN enable_network boolean DEFAULT true NOT NULL,
    ADD COLUMN enable_secrets boolean DEFAULT true NOT NULL,
    ADD COLUMN enable_exec boolean DEFAULT false NOT NULL;

UPDATE public.team_worker SET enable_network = enable_fetch, enable_secrets = enable_fetch;

ALTER TABLE public.team_worker DROP COLUMN enable_fetch;

UPDATE public.evaluation
SET waiting_reason = jsonb_set(waiting_reason, '{capability}', '"network"')
WHERE waiting_reason ->> 'capability' = 'fetch';
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
            "m20261018_000000_worker_network_capability is irreversible".into(),
        ))
    }
}
