/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// One multi-row enqueue gives every row the same next_attempt_at. The claim orders by
// (next_attempt_at, id) and an index on the timestamp alone sorts all of those ties per claim.
const UP: &str = r#"
DROP INDEX public."idx-pending_delivery-due";
CREATE INDEX "idx-pending_delivery-due" ON public.pending_delivery USING btree (next_attempt_at, id)
    WHERE ((delivered_at IS NULL) AND (failed_at IS NULL));
"#;

const DOWN: &str = r#"
DROP INDEX public."idx-pending_delivery-due";
CREATE INDEX "idx-pending_delivery-due" ON public.pending_delivery USING btree (next_attempt_at)
    WHERE ((delivered_at IS NULL) AND (failed_at IS NULL));
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
