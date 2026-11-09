/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "DELETE FROM public.worker_registration w \
    WHERE NOT EXISTS (SELECT 1 FROM public.project p WHERE p.id = w.peer_id); \
    ALTER TABLE ONLY public.worker_registration \
    ADD CONSTRAINT \"fk-worker_registration-peer_id\" FOREIGN KEY (peer_id) \
    REFERENCES public.project(id) ON DELETE CASCADE;";

const DOWN: &str = "ALTER TABLE ONLY public.worker_registration \
    DROP CONSTRAINT \"fk-worker_registration-peer_id\";";

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
