/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: [&str; 17] = [
    "CREATE TABLE team (
        id uuid PRIMARY KEY,
        name character varying NOT NULL UNIQUE,
        display_name text NOT NULL,
        oidc_group text,
        scim_group text,
        new_project_users boolean NOT NULL DEFAULT false,
        new_project_workers boolean NOT NULL DEFAULT false,
        new_project_role uuid REFERENCES role(id) ON DELETE SET NULL,
        created_by uuid REFERENCES \"user\"(id) ON DELETE SET NULL,
        managed boolean NOT NULL DEFAULT false,
        created_at timestamp without time zone NOT NULL
    )",
    "CREATE UNIQUE INDEX idx_team_scim_group ON team (scim_group) WHERE scim_group IS NOT NULL",
    "CREATE INDEX idx_team_oidc_group ON team (oidc_group) WHERE oidc_group IS NOT NULL",
    "CREATE TABLE team_user (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        \"user\" uuid NOT NULL REFERENCES \"user\"(id) ON DELETE CASCADE,
        role smallint NOT NULL,
        via_group boolean NOT NULL DEFAULT false,
        UNIQUE (team, \"user\")
    )",
    "CREATE INDEX idx_team_user_user ON team_user (\"user\")",
    "CREATE TABLE team_project (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        project uuid NOT NULL REFERENCES project(id) ON DELETE CASCADE,
        role uuid REFERENCES role(id) ON DELETE CASCADE,
        includes_users boolean NOT NULL,
        includes_workers boolean NOT NULL,
        created_at timestamp without time zone NOT NULL,
        UNIQUE (team, project),
        CHECK (includes_users OR includes_workers),
        CHECK (NOT includes_users OR role IS NOT NULL)
    )",
    "CREATE INDEX idx_team_project_project ON team_project (project)",
    "CREATE TABLE team_cache (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        cache uuid NOT NULL REFERENCES cache(id) ON DELETE CASCADE,
        role uuid NOT NULL REFERENCES cache_role(id) ON DELETE CASCADE,
        created_at timestamp without time zone NOT NULL,
        UNIQUE (team, cache)
    )",
    "CREATE TABLE team_invitation (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        \"user\" uuid NOT NULL REFERENCES \"user\"(id) ON DELETE CASCADE,
        role smallint NOT NULL,
        invited_by uuid NOT NULL REFERENCES \"user\"(id) ON DELETE CASCADE,
        token character varying NOT NULL UNIQUE,
        created_at timestamp without time zone NOT NULL,
        expires_at timestamp without time zone NOT NULL,
        UNIQUE (team, \"user\")
    )",
    "CREATE TABLE team_project_request (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        project uuid NOT NULL REFERENCES project(id) ON DELETE CASCADE,
        role uuid REFERENCES role(id) ON DELETE CASCADE,
        includes_users boolean NOT NULL,
        includes_workers boolean NOT NULL,
        requested_by uuid REFERENCES \"user\"(id) ON DELETE SET NULL,
        created_at timestamp without time zone NOT NULL,
        UNIQUE (team, project)
    )",
    "CREATE TABLE team_cache_request (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        cache uuid NOT NULL REFERENCES cache(id) ON DELETE CASCADE,
        role uuid NOT NULL REFERENCES cache_role(id) ON DELETE CASCADE,
        requested_by uuid REFERENCES \"user\"(id) ON DELETE SET NULL,
        created_at timestamp without time zone NOT NULL,
        UNIQUE (team, cache)
    )",
    "CREATE TABLE team_worker (
        id uuid PRIMARY KEY,
        team uuid NOT NULL REFERENCES team(id) ON DELETE CASCADE,
        worker_id character varying NOT NULL UNIQUE,
        token_hash character varying NOT NULL,
        token_encrypted text,
        url character varying,
        display_name text NOT NULL,
        gradient_ci boolean NOT NULL DEFAULT false,
        enable_fetch boolean NOT NULL DEFAULT true,
        enable_eval boolean NOT NULL DEFAULT true,
        enable_build boolean NOT NULL DEFAULT true,
        active boolean NOT NULL DEFAULT true,
        managed boolean NOT NULL DEFAULT false,
        created_by uuid REFERENCES \"user\"(id) ON DELETE SET NULL,
        created_at timestamp without time zone NOT NULL
    )",
    "CREATE INDEX idx_team_worker_team ON team_worker (team)",
    "CREATE UNIQUE INDEX idx_team_worker_one_gradient_ci ON team_worker (team) WHERE gradient_ci",
    "CREATE INDEX idx_team_cache_cache ON team_cache (cache)",
    "CREATE VIEW project_access AS
        SELECT pu.project, pu.\"user\", pu.role, false AS via_team FROM project_user pu
        UNION ALL
        SELECT tp.project, tu.\"user\", tp.role, true AS via_team
        FROM team_project tp JOIN team_user tu ON tu.team = tp.team
        WHERE tp.includes_users",
    "CREATE VIEW cache_access AS
        SELECT cu.cache, cu.\"user\", cu.role, false AS via_team FROM cache_user cu
        UNION ALL
        SELECT tc.cache, tu.\"user\", tc.role, true AS via_team
        FROM team_cache tc JOIN team_user tu ON tu.team = tc.team",
];

const DOWN: [&str; 10] = [
    "DROP VIEW cache_access",
    "DROP VIEW project_access",
    "DROP TABLE team_worker",
    "DROP TABLE team_cache_request",
    "DROP TABLE team_project_request",
    "DROP TABLE team_invitation",
    "DROP TABLE team_cache",
    "DROP TABLE team_project",
    "DROP TABLE team_user",
    "DROP TABLE team",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in UP {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in DOWN {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }
}
