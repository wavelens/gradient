/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.eval_set_cost (
    id uuid NOT NULL,
    task uuid NOT NULL,
    path text[] NOT NULL,
    startup_ms double precision,
    peak_rss_mb double precision NOT NULL,
    samples integer NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT eval_set_cost_pkey PRIMARY KEY (id),
    CONSTRAINT eval_set_cost_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX "uq-eval_set_cost-task-path" ON public.eval_set_cost USING btree (task, path);
CREATE TABLE public.eval_subtree_cost (
    id uuid NOT NULL,
    task uuid NOT NULL,
    path text[] NOT NULL,
    attrs double precision NOT NULL,
    ms_per_attr double precision NOT NULL,
    samples integer NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    CONSTRAINT eval_subtree_cost_pkey PRIMARY KEY (id),
    CONSTRAINT eval_subtree_cost_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX "uq-eval_subtree_cost-task-path" ON public.eval_subtree_cost USING btree (task, path);
"#;

const DOWN: &str = r#"
DROP TABLE public.eval_subtree_cost;
DROP TABLE public.eval_set_cost;
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
