/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cluster jobs. The unique open-attempt index is the cross-instance arbiter for
//! a cluster claim, as `idx-dispatched_job-open-job` is for a single job. The
//! `dispatched_job` foreign key is validated separately, off the exclusive lock.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS cluster_job (
        id uuid PRIMARY KEY,
        status smallint NOT NULL DEFAULT 0,
        same_zone boolean NOT NULL DEFAULT false,
        attempts integer NOT NULL DEFAULT 0,
        retry_budget integer NOT NULL,
        created_at timestamp NOT NULL,
        updated_at timestamp NOT NULL)"#,
    r#"CREATE TABLE IF NOT EXISTS cluster_member (
        id uuid PRIMARY KEY,
        cluster_job uuid NOT NULL REFERENCES cluster_job(id) ON DELETE CASCADE,
        evaluation uuid REFERENCES evaluation(id) ON DELETE CASCADE,
        derivation_build uuid REFERENCES derivation_build(id) ON DELETE CASCADE,
        role text NOT NULL,
        "primary" boolean NOT NULL DEFAULT false,
        pin text,
        CHECK (num_nonnulls(evaluation, derivation_build) = 1))"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cluster_member-cluster_job" ON cluster_member (cluster_job)"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-cluster_member-evaluation"
       ON cluster_member (evaluation) WHERE evaluation IS NOT NULL"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-cluster_member-derivation_build"
       ON cluster_member (derivation_build) WHERE derivation_build IS NOT NULL"#,
    r#"CREATE TABLE IF NOT EXISTS cluster_attempt (
        id uuid PRIMARY KEY,
        cluster_job uuid NOT NULL REFERENCES cluster_job(id) ON DELETE CASCADE,
        created_at timestamp NOT NULL,
        started_at timestamp,
        finished_at timestamp,
        outcome smallint)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cluster_attempt-cluster_job" ON cluster_attempt (cluster_job)"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx-cluster_attempt-open"
       ON cluster_attempt (cluster_job) WHERE finished_at IS NULL"#,
    "ALTER TABLE dispatched_job ADD COLUMN IF NOT EXISTS cluster_attempt uuid",
    r#"ALTER TABLE dispatched_job ADD CONSTRAINT "fk-dispatched_job-cluster_attempt"
       FOREIGN KEY (cluster_attempt) REFERENCES cluster_attempt(id) ON DELETE SET NULL NOT VALID"#,
    r#"ALTER TABLE dispatched_job VALIDATE CONSTRAINT "fk-dispatched_job-cluster_attempt""#,
    r#"CREATE INDEX IF NOT EXISTS "idx-dispatched_job-cluster_attempt"
       ON dispatched_job (cluster_attempt) WHERE cluster_attempt IS NOT NULL"#,
];

const DOWN: &[&str] = &[
    "ALTER TABLE dispatched_job DROP COLUMN IF EXISTS cluster_attempt",
    "DROP TABLE IF EXISTS cluster_attempt",
    "DROP TABLE IF EXISTS cluster_member",
    "DROP TABLE IF EXISTS cluster_job",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
