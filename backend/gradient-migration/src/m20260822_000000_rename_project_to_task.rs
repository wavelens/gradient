/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Renames the `project` entity to `task`. Postgres carries table and column
//! renames through to dependent objects but leaves index and constraint names
//! alone, so all 23 of those are renamed explicitly here. Postgres 17+ also
//! catalogues an auto-named constraint per NOT NULL column, so a final block
//! sweeps those by substitution, scoped to the affected tables so sibling
//! tables such as `admin_task` are left alone.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE project RENAME TO task;
ALTER TABLE project_action RENAME TO task_action;
ALTER TABLE project_action_delivery RENAME TO task_action_delivery;
ALTER TABLE project_flake_input_override RENAME TO task_flake_input_override;
ALTER TABLE project_trigger RENAME TO task_trigger;

ALTER TABLE dispatched_job RENAME COLUMN project TO task;
ALTER TABLE entry_point RENAME COLUMN project TO task;
ALTER TABLE evaluation RENAME COLUMN project TO task;
ALTER TABLE open_pr_state RENAME COLUMN project TO task;
ALTER TABLE task_action RENAME COLUMN project TO task;
ALTER TABLE task_flake_input_override RENAME COLUMN project TO task;
ALTER TABLE task_trigger RENAME COLUMN project TO task;

ALTER TABLE task RENAME CONSTRAINT project_pkey TO task_pkey;
ALTER TABLE task_action RENAME CONSTRAINT project_action_pkey TO task_action_pkey;
ALTER TABLE task_action_delivery RENAME CONSTRAINT project_action_delivery_pkey TO task_action_delivery_pkey;
ALTER TABLE task_flake_input_override RENAME CONSTRAINT project_flake_input_override_pkey TO task_flake_input_override_pkey;
ALTER TABLE task_trigger RENAME CONSTRAINT project_trigger_pkey TO task_trigger_pkey;

ALTER TABLE entry_point RENAME CONSTRAINT "fk-entry_point-project" TO "fk-entry_point-task";
ALTER TABLE evaluation RENAME CONSTRAINT "fk-evaluation-project" TO "fk-evaluation-task";
ALTER TABLE task RENAME CONSTRAINT "fk-project-created_by" TO "fk-task-created_by";
ALTER TABLE task RENAME CONSTRAINT "fk-project-organization" TO "fk-task-organization";
ALTER TABLE task_action RENAME CONSTRAINT "fk-project_action-created_by" TO "fk-task_action-created_by";
ALTER TABLE task_action RENAME CONSTRAINT "fk-project_action-project" TO "fk-task_action-task";
ALTER TABLE task_action_delivery RENAME CONSTRAINT "fk-project_action_delivery-action_id" TO "fk-task_action_delivery-action_id";
ALTER TABLE task_trigger RENAME CONSTRAINT "fk-project_trigger-project" TO "fk-task_trigger-task";
ALTER TABLE open_pr_state RENAME CONSTRAINT open_pr_state_project_fkey TO open_pr_state_task_fkey;
ALTER TABLE task_flake_input_override RENAME CONSTRAINT project_flake_input_override_project_fkey TO task_flake_input_override_task_fkey;

ALTER INDEX idx_project_action_delivery_action_delivered RENAME TO idx_task_action_delivery_action_delivered;
ALTER INDEX idx_project_action_project_name RENAME TO idx_task_action_task_name;
ALTER INDEX idx_project_trigger_project_active RENAME TO idx_task_trigger_task_active;
ALTER INDEX idx_project_trigger_type_active RENAME TO idx_task_trigger_type_active;
ALTER INDEX uq_evaluation_one_active_per_project RENAME TO uq_evaluation_one_active_per_task;
ALTER INDEX "uq-open_pr_state-project-action-branch" RENAME TO "uq-open_pr_state-task-action-branch";
ALTER INDEX "uq-project_flake_input_override-project-input_name" RENAME TO "uq-task_flake_input_override-task-input_name";

DO $$
DECLARE r record;
BEGIN
    FOR r IN
        SELECT t.relname AS tbl, c.conname AS name
        FROM pg_constraint c
        JOIN pg_class t ON t.oid = c.conrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        WHERE n.nspname = 'public'
          AND c.contype = 'n'
          AND c.conname LIKE '%project%'
          AND t.relname IN ('task', 'task_action', 'task_action_delivery',
                            'task_flake_input_override', 'task_trigger',
                            'dispatched_job', 'entry_point', 'evaluation',
                            'open_pr_state')
    LOOP
        EXECUTE format('ALTER TABLE public.%I RENAME CONSTRAINT %I TO %I',
                       r.tbl, r.name, replace(r.name, 'project', 'task'));
    END LOOP;
END $$;
"#;

const DOWN: &str = r#"
DO $$
DECLARE r record;
BEGIN
    FOR r IN
        SELECT t.relname AS tbl, c.conname AS name
        FROM pg_constraint c
        JOIN pg_class t ON t.oid = c.conrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        WHERE n.nspname = 'public'
          AND c.contype = 'n'
          AND c.conname LIKE '%task%'
          AND t.relname IN ('task', 'task_action', 'task_action_delivery',
                            'task_flake_input_override', 'task_trigger',
                            'dispatched_job', 'entry_point', 'evaluation',
                            'open_pr_state')
    LOOP
        EXECUTE format('ALTER TABLE public.%I RENAME CONSTRAINT %I TO %I',
                       r.tbl, r.name, replace(r.name, 'task', 'project'));
    END LOOP;
END $$;

ALTER INDEX "uq-task_flake_input_override-task-input_name" RENAME TO "uq-project_flake_input_override-project-input_name";
ALTER INDEX "uq-open_pr_state-task-action-branch" RENAME TO "uq-open_pr_state-project-action-branch";
ALTER INDEX uq_evaluation_one_active_per_task RENAME TO uq_evaluation_one_active_per_project;
ALTER INDEX idx_task_trigger_type_active RENAME TO idx_project_trigger_type_active;
ALTER INDEX idx_task_trigger_task_active RENAME TO idx_project_trigger_project_active;
ALTER INDEX idx_task_action_task_name RENAME TO idx_project_action_project_name;
ALTER INDEX idx_task_action_delivery_action_delivered RENAME TO idx_project_action_delivery_action_delivered;

ALTER TABLE task_flake_input_override RENAME CONSTRAINT task_flake_input_override_task_fkey TO project_flake_input_override_project_fkey;
ALTER TABLE open_pr_state RENAME CONSTRAINT open_pr_state_task_fkey TO open_pr_state_project_fkey;
ALTER TABLE task_trigger RENAME CONSTRAINT "fk-task_trigger-task" TO "fk-project_trigger-project";
ALTER TABLE task_action_delivery RENAME CONSTRAINT "fk-task_action_delivery-action_id" TO "fk-project_action_delivery-action_id";
ALTER TABLE task_action RENAME CONSTRAINT "fk-task_action-task" TO "fk-project_action-project";
ALTER TABLE task_action RENAME CONSTRAINT "fk-task_action-created_by" TO "fk-project_action-created_by";
ALTER TABLE task RENAME CONSTRAINT "fk-task-organization" TO "fk-project-organization";
ALTER TABLE task RENAME CONSTRAINT "fk-task-created_by" TO "fk-project-created_by";
ALTER TABLE evaluation RENAME CONSTRAINT "fk-evaluation-task" TO "fk-evaluation-project";
ALTER TABLE entry_point RENAME CONSTRAINT "fk-entry_point-task" TO "fk-entry_point-project";

ALTER TABLE task_trigger RENAME CONSTRAINT task_trigger_pkey TO project_trigger_pkey;
ALTER TABLE task_flake_input_override RENAME CONSTRAINT task_flake_input_override_pkey TO project_flake_input_override_pkey;
ALTER TABLE task_action_delivery RENAME CONSTRAINT task_action_delivery_pkey TO project_action_delivery_pkey;
ALTER TABLE task_action RENAME CONSTRAINT task_action_pkey TO project_action_pkey;
ALTER TABLE task RENAME CONSTRAINT task_pkey TO project_pkey;

ALTER TABLE task_trigger RENAME COLUMN task TO project;
ALTER TABLE task_flake_input_override RENAME COLUMN task TO project;
ALTER TABLE task_action RENAME COLUMN task TO project;
ALTER TABLE open_pr_state RENAME COLUMN task TO project;
ALTER TABLE evaluation RENAME COLUMN task TO project;
ALTER TABLE entry_point RENAME COLUMN task TO project;
ALTER TABLE dispatched_job RENAME COLUMN task TO project;

ALTER TABLE task_trigger RENAME TO project_trigger;
ALTER TABLE task_flake_input_override RENAME TO project_flake_input_override;
ALTER TABLE task_action_delivery RENAME TO project_action_delivery;
ALTER TABLE task_action RENAME TO project_action;
ALTER TABLE task RENAME TO project;
"#;

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
