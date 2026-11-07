/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "CREATE TABLE public.incoming_webhook_delivery ( \
        id uuid PRIMARY KEY, \
        integration uuid NOT NULL REFERENCES public.integration(id) ON DELETE CASCADE, \
        event character varying NOT NULL, \
        status_code smallint NOT NULL, \
        result smallint NOT NULL, \
        detail text, \
        repository character varying, \
        tasks_scanned integer NOT NULL DEFAULT 0, \
        queued integer NOT NULL DEFAULT 0, \
        created_at timestamp without time zone NOT NULL \
    ); \
    CREATE INDEX incoming_webhook_delivery_integration_created_at \
        ON public.incoming_webhook_delivery (integration, created_at DESC);";

const DOWN: &str = "DROP TABLE public.incoming_webhook_delivery;";

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
