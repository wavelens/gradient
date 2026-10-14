/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Backfill: polled branches get `refs/heads/<branch>`, out-of-band runs get `none`, all other rows
// get the default branch key `''`.
const UP: &str = r#"
ALTER TABLE public.evaluation ADD COLUMN branch text DEFAULT '' NOT NULL;

UPDATE public.evaluation AS e
SET branch = 'refs/heads/' || (t.config ->> 'branch')
FROM public.task_trigger AS t
WHERE e.trigger = t.id
  AND t.trigger_type = 0
  AND coalesce(t.config ->> 'branch', '') <> '';

UPDATE public.evaluation
SET branch = 'none'
WHERE kind IN (1, 3) OR (concurrent AND trigger IS NULL);

DROP INDEX public.uq_evaluation_one_active_per_task;

CREATE UNIQUE INDEX uq_evaluation_one_active_per_branch ON public.evaluation USING btree (task, branch)
WHERE ((task IS NOT NULL) AND (status = ANY (ARRAY[0, 1, 2, 3, 4, 8])) AND (NOT concurrent));
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
            "m20261014_000000_evaluation_branch is irreversible".into(),
        ))
    }
}
