/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Renames the `organization` entity to `project`. Postgres carries table and
//! column renames through to dependent objects but leaves index and constraint
//! names alone, so all 27 of those are renamed explicitly here. Postgres 17+
//! also catalogues an auto-named constraint per NOT NULL column, so a final
//! block sweeps those by substitution, scoped to the affected tables.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE organization RENAME TO project;
ALTER TABLE organization_base_worker RENAME TO project_base_worker;
ALTER TABLE organization_cache RENAME TO project_cache;
ALTER TABLE organization_user RENAME TO project_user;

ALTER TABLE api RENAME COLUMN organization TO project;
ALTER TABLE build_request_blob RENAME COLUMN organization TO project;
ALTER TABLE dispatched_job RENAME COLUMN organization TO project;
ALTER TABLE github_installation RENAME COLUMN organization TO project;
ALTER TABLE integration RENAME COLUMN organization TO project;
ALTER TABLE project_base_worker RENAME COLUMN organization TO project;
ALTER TABLE project_cache RENAME COLUMN organization TO project;
ALTER TABLE project_user RENAME COLUMN organization TO project;
ALTER TABLE role RENAME COLUMN organization TO project;
ALTER TABLE task RENAME COLUMN organization TO project;
ALTER TABLE upload_session RENAME COLUMN organization TO project;
ALTER TABLE worker_connection RENAME COLUMN organization TO project;
ALTER TABLE worker_sample RENAME COLUMN organization TO project;

ALTER TABLE project RENAME CONSTRAINT organization_pkey TO project_pkey;
ALTER TABLE project_base_worker RENAME CONSTRAINT organization_base_worker_pkey TO project_base_worker_pkey;
ALTER TABLE project_cache RENAME CONSTRAINT organization_cache_pkey TO project_cache_pkey;
ALTER TABLE project_user RENAME CONSTRAINT organization_user_pkey TO project_user_pkey;
ALTER TABLE project RENAME CONSTRAINT organization_name_key TO project_name_key;

ALTER TABLE api RENAME CONSTRAINT fk_api_organization TO fk_api_project;
ALTER TABLE build_request_blob RENAME CONSTRAINT "fk-build_request_blob-organization" TO "fk-build_request_blob-project";
ALTER TABLE github_installation RENAME CONSTRAINT "fk-github_installation-organization" TO "fk-github_installation-project";
ALTER TABLE integration RENAME CONSTRAINT "fk-integration-organization" TO "fk-integration-project";
ALTER TABLE project RENAME CONSTRAINT "fk-organization-created_by" TO "fk-project-created_by";
ALTER TABLE project_base_worker RENAME CONSTRAINT "fk-org_base_worker-base_worker" TO "fk-project_base_worker-base_worker";
ALTER TABLE project_base_worker RENAME CONSTRAINT "fk-org_base_worker-created_by" TO "fk-project_base_worker-created_by";
ALTER TABLE project_base_worker RENAME CONSTRAINT "fk-org_base_worker-organization" TO "fk-project_base_worker-project";
ALTER TABLE project_cache RENAME CONSTRAINT "fk-organization_cache-cache" TO "fk-project_cache-cache";
ALTER TABLE project_cache RENAME CONSTRAINT "fk-organization_cache-organization" TO "fk-project_cache-project";
ALTER TABLE project_user RENAME CONSTRAINT "fk-organization_user-organization" TO "fk-project_user-project";
ALTER TABLE project_user RENAME CONSTRAINT "fk-organization_user-role" TO "fk-project_user-role";
ALTER TABLE project_user RENAME CONSTRAINT "fk-organization_user-user" TO "fk-project_user-user";
ALTER TABLE role RENAME CONSTRAINT "fk-role-organization" TO "fk-role-project";
ALTER TABLE task RENAME CONSTRAINT "fk-task-organization" TO "fk-task-project";
ALTER TABLE upload_session RENAME CONSTRAINT "fk-upload_session-organization" TO "fk-upload_session-project";

ALTER INDEX "idx-dispatched_job-org-dispatched_at" RENAME TO "idx-dispatched_job-project-dispatched_at";
ALTER INDEX "idx-github-installation-org-installation" RENAME TO "idx-github-installation-project-installation";
ALTER INDEX "idx-integration-org-kind-name" RENAME TO "idx-integration-project-kind-name";
ALTER INDEX idx_api_organization RENAME TO idx_api_project;
ALTER INDEX idx_org_base_worker_unique RENAME TO idx_project_base_worker_unique;
ALTER INDEX "ux-build_request_blob-org-hash" RENAME TO "ux-build_request_blob-project-hash";

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
          AND c.conname LIKE '%organization%'
          AND t.relname IN ('project', 'project_base_worker', 'project_cache',
                            'project_user', 'api', 'build_request_blob',
                            'dispatched_job', 'github_installation',
                            'integration', 'role', 'task', 'upload_session',
                            'worker_connection', 'worker_sample')
    LOOP
        EXECUTE format('ALTER TABLE public.%I RENAME CONSTRAINT %I TO %I',
                       r.tbl, r.name, replace(r.name, 'organization', 'project'));
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
          AND c.conname LIKE '%project%'
          AND t.relname IN ('project', 'project_base_worker', 'project_cache',
                            'project_user', 'api', 'build_request_blob',
                            'dispatched_job', 'github_installation',
                            'integration', 'role', 'task', 'upload_session',
                            'worker_connection', 'worker_sample')
    LOOP
        EXECUTE format('ALTER TABLE public.%I RENAME CONSTRAINT %I TO %I',
                       r.tbl, r.name, replace(r.name, 'project', 'organization'));
    END LOOP;
END $$;

ALTER INDEX "ux-build_request_blob-project-hash" RENAME TO "ux-build_request_blob-org-hash";
ALTER INDEX idx_project_base_worker_unique RENAME TO idx_org_base_worker_unique;
ALTER INDEX idx_api_project RENAME TO idx_api_organization;
ALTER INDEX "idx-integration-project-kind-name" RENAME TO "idx-integration-org-kind-name";
ALTER INDEX "idx-github-installation-project-installation" RENAME TO "idx-github-installation-org-installation";
ALTER INDEX "idx-dispatched_job-project-dispatched_at" RENAME TO "idx-dispatched_job-org-dispatched_at";

ALTER TABLE upload_session RENAME CONSTRAINT "fk-upload_session-project" TO "fk-upload_session-organization";
ALTER TABLE task RENAME CONSTRAINT "fk-task-project" TO "fk-task-organization";
ALTER TABLE role RENAME CONSTRAINT "fk-role-project" TO "fk-role-organization";
ALTER TABLE project_user RENAME CONSTRAINT "fk-project_user-user" TO "fk-organization_user-user";
ALTER TABLE project_user RENAME CONSTRAINT "fk-project_user-role" TO "fk-organization_user-role";
ALTER TABLE project_user RENAME CONSTRAINT "fk-project_user-project" TO "fk-organization_user-organization";
ALTER TABLE project_cache RENAME CONSTRAINT "fk-project_cache-project" TO "fk-organization_cache-organization";
ALTER TABLE project_cache RENAME CONSTRAINT "fk-project_cache-cache" TO "fk-organization_cache-cache";
ALTER TABLE project_base_worker RENAME CONSTRAINT "fk-project_base_worker-project" TO "fk-org_base_worker-organization";
ALTER TABLE project_base_worker RENAME CONSTRAINT "fk-project_base_worker-created_by" TO "fk-org_base_worker-created_by";
ALTER TABLE project_base_worker RENAME CONSTRAINT "fk-project_base_worker-base_worker" TO "fk-org_base_worker-base_worker";
ALTER TABLE project RENAME CONSTRAINT "fk-project-created_by" TO "fk-organization-created_by";
ALTER TABLE integration RENAME CONSTRAINT "fk-integration-project" TO "fk-integration-organization";
ALTER TABLE github_installation RENAME CONSTRAINT "fk-github_installation-project" TO "fk-github_installation-organization";
ALTER TABLE build_request_blob RENAME CONSTRAINT "fk-build_request_blob-project" TO "fk-build_request_blob-organization";
ALTER TABLE api RENAME CONSTRAINT fk_api_project TO fk_api_organization;

ALTER TABLE project RENAME CONSTRAINT project_name_key TO organization_name_key;
ALTER TABLE project_user RENAME CONSTRAINT project_user_pkey TO organization_user_pkey;
ALTER TABLE project_cache RENAME CONSTRAINT project_cache_pkey TO organization_cache_pkey;
ALTER TABLE project_base_worker RENAME CONSTRAINT project_base_worker_pkey TO organization_base_worker_pkey;
ALTER TABLE project RENAME CONSTRAINT project_pkey TO organization_pkey;

ALTER TABLE worker_sample RENAME COLUMN project TO organization;
ALTER TABLE worker_connection RENAME COLUMN project TO organization;
ALTER TABLE upload_session RENAME COLUMN project TO organization;
ALTER TABLE task RENAME COLUMN project TO organization;
ALTER TABLE role RENAME COLUMN project TO organization;
ALTER TABLE project_user RENAME COLUMN project TO organization;
ALTER TABLE project_cache RENAME COLUMN project TO organization;
ALTER TABLE project_base_worker RENAME COLUMN project TO organization;
ALTER TABLE integration RENAME COLUMN project TO organization;
ALTER TABLE github_installation RENAME COLUMN project TO organization;
ALTER TABLE dispatched_job RENAME COLUMN project TO organization;
ALTER TABLE build_request_blob RENAME COLUMN project TO organization;
ALTER TABLE api RENAME COLUMN project TO organization;

ALTER TABLE project_user RENAME TO organization_user;
ALTER TABLE project_cache RENAME TO organization_cache;
ALTER TABLE project_base_worker RENAME TO organization_base_worker;
ALTER TABLE project RENAME TO organization;
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
