/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.pull_request_summary (
    id uuid NOT NULL,
    task uuid NOT NULL,
    pr_number bigint NOT NULL,
    comment_id bigint,
    CONSTRAINT pull_request_summary_pkey PRIMARY KEY (id),
    CONSTRAINT pull_request_summary_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX "uq-pull_request_summary-task-pr_number" ON public.pull_request_summary USING btree (task, pr_number);
"#;

const DOWN: &str = r#"
DROP TABLE public.pull_request_summary;
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
