/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
DELETE FROM public.evaluation_metric m
USING public.evaluation_metric newer
WHERE newer.evaluation = m.evaluation
  AND (newer.created_at, newer.id) > (m.created_at, m.id);
DROP INDEX IF EXISTS public."idx-evaluation_metric-evaluation";
CREATE UNIQUE INDEX "uq-evaluation_metric-evaluation" ON public.evaluation_metric USING btree (evaluation);
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS public."uq-evaluation_metric-evaluation";
CREATE INDEX "idx-evaluation_metric-evaluation" ON public.evaluation_metric USING btree (evaluation);
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
