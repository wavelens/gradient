/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Servers built from main before this change need the token column during the rolling upgrade.
// Servers of this version don't write that column. Their expiry sweeps clear that column. The next
// release: column removal.
const UP: &str = "SET LOCAL lock_timeout = '5s'; \
    ALTER TABLE public.session \
    ADD COLUMN is_cli_device_sign_in_session boolean NOT NULL DEFAULT false; \
    DELETE FROM public.cli_device_authorization; \
    ALTER TABLE public.cli_device_authorization ADD COLUMN approving_session_id uuid \
    CONSTRAINT \"fk-cli-device-auth-approving-session\" \
    REFERENCES public.session (id) ON DELETE CASCADE;";

const DOWN: &str = "SET LOCAL lock_timeout = '5s'; \
    ALTER TABLE public.cli_device_authorization DROP COLUMN approving_session_id; \
    ALTER TABLE public.session DROP COLUMN is_cli_device_sign_in_session;";

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
