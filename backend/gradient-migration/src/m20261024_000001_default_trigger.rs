/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// A task without triggers gets an inactive polling trigger, so a manual start keeps its old
// behaviour. Each task's oldest polling trigger, else its oldest trigger, becomes the default.
const UP: &str = r#"
ALTER TABLE public.task_trigger ADD COLUMN is_default boolean DEFAULT false NOT NULL;

INSERT INTO public.task_trigger (id, task, trigger_type, config, active, created_at, updated_at)
SELECT gen_random_uuid(), t.id, 0, '{"interval_secs": 300}'::jsonb, false, now(), now()
FROM public.task AS t
WHERE NOT EXISTS (SELECT 1 FROM public.task_trigger AS tt WHERE tt.task = t.id);

UPDATE public.task_trigger AS tt
SET is_default = true
FROM (
    SELECT DISTINCT ON (task) id
    FROM public.task_trigger
    ORDER BY task, trigger_type <> 0, created_at, id
) AS first
WHERE tt.id = first.id;

CREATE UNIQUE INDEX uq_task_trigger_one_default_per_task ON public.task_trigger USING btree (task)
WHERE is_default;
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
            "m20261024_000001_default_trigger is irreversible".into(),
        ))
    }
}
