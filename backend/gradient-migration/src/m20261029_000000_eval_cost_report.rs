/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.eval_cost_report (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    job text NOT NULL,
    report jsonb NOT NULL,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT eval_cost_report_pkey PRIMARY KEY (id),
    CONSTRAINT eval_cost_report_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE
);
CREATE INDEX "idx-eval_cost_report-evaluation" ON public.eval_cost_report USING btree (evaluation);
"#;

const DOWN: &str = "DROP TABLE public.eval_cost_report;";

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
