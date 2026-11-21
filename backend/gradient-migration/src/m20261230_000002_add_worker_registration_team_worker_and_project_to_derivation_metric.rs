/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Existing rows keep NULL in all three columns. These legacy samples have no known worker or project.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.derivation_metric
    ADD COLUMN worker_registration uuid REFERENCES public.worker_registration(id) ON DELETE SET NULL,
    ADD COLUMN team_worker uuid REFERENCES public.team_worker(id) ON DELETE SET NULL,
    ADD COLUMN project uuid REFERENCES public.project(id) ON DELETE CASCADE;
"#;

const DOWN: &str = r#"
SET LOCAL lock_timeout = '5s';
ALTER TABLE public.derivation_metric
    DROP COLUMN project,
    DROP COLUMN team_worker,
    DROP COLUMN worker_registration;
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
