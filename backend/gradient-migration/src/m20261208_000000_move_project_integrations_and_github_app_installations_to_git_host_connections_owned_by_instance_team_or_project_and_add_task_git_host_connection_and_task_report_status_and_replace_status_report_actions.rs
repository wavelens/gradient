/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Project integrations each map to a connection to a git host for their project. An inbound integration and an outbound integration with the same name in one project merge into one connection. That connection can receive events and report status. Installations of the GitHub App each map to one connection for the instance. They replace the GitHub rows for each project. Tasks get the connection for the action that reports status. Tasks without that action get the connection for the first push trigger or the first pull request trigger. Actions that report status map to task.report_status. Down cannot restore connections for a team or an instance, or installations of the GitHub App. Down must remove those rows.
const UP: &str = r#"
ALTER TABLE public.integration RENAME TO git_host_connection;
ALTER TABLE public.git_host_connection RENAME CONSTRAINT integration_pkey TO git_host_connection_pkey;
ALTER TABLE public.git_host_connection RENAME CONSTRAINT "fk-integration-created_by" TO "fk-git_host_connection-created_by";
ALTER TABLE public.git_host_connection RENAME CONSTRAINT "fk-integration-project" TO "fk-git_host_connection-project";
ALTER TABLE public.git_host_connection RENAME COLUMN endpoint_url TO base_url;
ALTER TABLE public.git_host_connection ALTER COLUMN project DROP NOT NULL;
ALTER TABLE public.git_host_connection
    ADD COLUMN owner_team uuid REFERENCES public.team(id) ON UPDATE CASCADE ON DELETE CASCADE,
    ADD COLUMN receives_events boolean DEFAULT false NOT NULL,
    ADD COLUMN reports_status boolean DEFAULT false NOT NULL,
    ADD COLUMN github_installation_id bigint,
    ADD COLUMN account_login text;
UPDATE public.git_host_connection SET receives_events = (kind = 0), reports_status = (kind = 1);

CREATE TEMP TABLE connection_replacement (old_id uuid PRIMARY KEY, new_id uuid NOT NULL) ON COMMIT DROP;

INSERT INTO public.git_host_connection
    (id, project, name, display_name, kind, git_host_type, created_by, created_at, managed,
     receives_events, reports_status, github_installation_id, account_login)
SELECT gen_random_uuid(), NULL,
    CASE WHEN count(*) OVER (PARTITION BY base_name) > 1
        THEN 'github-' || installation_id ELSE base_name END,
    'GitHub', 0, 3, created_by, created_at, false, true, true, installation_id, account_login
FROM (
    SELECT DISTINCT ON (gi.installation_id) gi.installation_id, gi.account_login, gi.created_by,
        gi.created_at,
        CASE WHEN coalesce(trim(gi.account_login), '') <> ''
            THEN 'github-' || lower(trim(gi.account_login))
            ELSE 'github-' || gi.installation_id END AS base_name
    FROM public.github_installation gi
    ORDER BY gi.installation_id, gi.created_at
) installations;

INSERT INTO connection_replacement (old_id, new_id)
SELECT c.id, n.id
FROM public.git_host_connection c
JOIN public.github_installation gi ON gi.id = c.github_installation
JOIN public.git_host_connection n ON n.github_installation_id = gi.installation_id;

INSERT INTO connection_replacement (old_id, new_id)
SELECT o.id, i.id
FROM public.git_host_connection o
JOIN public.git_host_connection i
    ON i.project = o.project AND i.name = o.name AND i.git_host_type = o.git_host_type
    AND i.kind = 0 AND o.kind = 1
WHERE o.github_installation IS NULL AND i.github_installation IS NULL;

UPDATE public.git_host_connection i
SET reports_status = true,
    access_token = coalesce(i.access_token, o.access_token),
    base_url = coalesce(i.base_url, o.base_url)
FROM connection_replacement r
JOIN public.git_host_connection o ON o.id = r.old_id
WHERE i.id = r.new_id AND i.github_installation_id IS NULL;

UPDATE public.exec_job_pull_request p SET integration = r.new_id
FROM connection_replacement r WHERE p.integration = r.old_id;
UPDATE public.incoming_webhook_delivery d SET integration = r.new_id
FROM connection_replacement r WHERE d.integration = r.old_id;
UPDATE public.task_action a SET config = jsonb_set(a.config, '{integration_id}', to_jsonb(r.new_id::text))
FROM connection_replacement r WHERE (a.config->>'integration_id')::uuid = r.old_id;
UPDATE public.task_trigger t SET config = jsonb_set(t.config, '{integration_id}', to_jsonb(r.new_id::text))
FROM connection_replacement r WHERE (t.config->>'integration_id')::uuid = r.old_id;

DELETE FROM public.git_host_connection c USING connection_replacement r WHERE c.id = r.old_id;

UPDATE public.git_host_connection c SET name = c.name || '-' || left(c.id::text, 8)
WHERE c.kind = 1 AND EXISTS (
    SELECT 1 FROM public.git_host_connection other
    WHERE other.project = c.project AND other.name = c.name AND other.id <> c.id);

ALTER TABLE public.git_host_connection DROP COLUMN github_installation;
DROP TABLE public.github_installation;
DROP INDEX public."idx-integration-project-kind-name";
ALTER TABLE public.git_host_connection DROP COLUMN kind;
ALTER TABLE public.git_host_connection
    ADD CONSTRAINT git_host_connection_one_owner CHECK (project IS NULL OR owner_team IS NULL);
CREATE UNIQUE INDEX git_host_connection_project_name ON public.git_host_connection (project, name)
    WHERE project IS NOT NULL;
CREATE UNIQUE INDEX git_host_connection_team_name ON public.git_host_connection (owner_team, name)
    WHERE owner_team IS NOT NULL;
CREATE UNIQUE INDEX git_host_connection_instance_name ON public.git_host_connection (name)
    WHERE project IS NULL AND owner_team IS NULL;
CREATE UNIQUE INDEX git_host_connection_github_installation_id
    ON public.git_host_connection (github_installation_id) WHERE github_installation_id IS NOT NULL;

ALTER TABLE public.exec_job_pull_request RENAME COLUMN integration TO git_host_connection;
ALTER TABLE public.exec_job_pull_request
    RENAME CONSTRAINT "fk-exec_job_pull_request-integration" TO "fk-exec_job_pull_request-git_host_connection";
ALTER INDEX public."idx-exec_job_pull_request-integration" RENAME TO "idx-exec_job_pull_request-git_host_connection";
ALTER TABLE public.incoming_webhook_delivery RENAME COLUMN integration TO git_host_connection;
ALTER INDEX public.incoming_webhook_delivery_integration_created_at
    RENAME TO incoming_webhook_delivery_git_host_connection_created_at;

ALTER TABLE public.task
    ADD COLUMN git_host_connection uuid
        REFERENCES public.git_host_connection(id) ON UPDATE CASCADE ON DELETE SET NULL,
    ADD COLUMN report_status boolean DEFAULT false NOT NULL;
CREATE INDEX idx_task_git_host_connection ON public.task (git_host_connection);

UPDATE public.task t SET git_host_connection = a.connection, report_status = a.active
FROM (
    SELECT DISTINCT ON (task) task, (config->>'integration_id')::uuid AS connection, active
    FROM public.task_action WHERE action_type = 2
    ORDER BY task, active DESC, created_at
) a
JOIN public.git_host_connection c ON c.id = a.connection
WHERE a.task = t.id;

UPDATE public.task t SET git_host_connection = tr.connection
FROM (
    SELECT DISTINCT ON (task) task, (config->>'integration_id')::uuid AS connection
    FROM public.task_trigger WHERE trigger_type IN (1, 2) AND config ? 'integration_id'
    ORDER BY task, active DESC, created_at
) tr
JOIN public.git_host_connection c ON c.id = tr.connection
WHERE tr.task = t.id AND t.git_host_connection IS NULL;

UPDATE public.task_trigger SET config = config - 'integration_id' WHERE config ? 'integration_id';

CREATE TABLE public.task_commit_status_post_attempt (
    id uuid PRIMARY KEY,
    task uuid NOT NULL REFERENCES public.task(id) ON UPDATE CASCADE ON DELETE CASCADE,
    git_host_connection uuid
        REFERENCES public.git_host_connection(id) ON UPDATE CASCADE ON DELETE SET NULL,
    event character varying NOT NULL,
    response_status integer,
    response_body text,
    error_message text,
    success boolean NOT NULL,
    duration_ms integer NOT NULL,
    delivered_at timestamp without time zone NOT NULL
);
CREATE INDEX task_commit_status_post_attempt_task_delivered_at
    ON public.task_commit_status_post_attempt (task, delivered_at DESC);

INSERT INTO public.task_commit_status_post_attempt
    (id, task, git_host_connection, event, response_status, response_body, error_message, success,
     duration_ms, delivered_at)
SELECT d.id, a.task, t.git_host_connection, d.event, d.response_status, d.response_body,
    d.error_message, d.success, d.duration_ms, d.delivered_at
FROM public.task_action_delivery d
JOIN public.task_action a ON a.id = d.action_id
JOIN public.task t ON t.id = a.task
WHERE a.action_type = 2;

DELETE FROM public.task_action WHERE action_type = 2;
UPDATE public.task_action
SET config = (config - 'integration_id') || jsonb_build_object('git_host_connection_id', config->'integration_id')
WHERE action_type = 3 AND config ? 'integration_id';
"#;

const DOWN: &str = r#"
UPDATE public.task_action
SET config = (config - 'git_host_connection_id') || jsonb_build_object('integration_id', config->'git_host_connection_id')
WHERE action_type = 3 AND config ? 'git_host_connection_id';
DELETE FROM public.git_host_connection WHERE project IS NULL;

CREATE TEMP TABLE outbound_copy (connection uuid PRIMARY KEY, copy uuid NOT NULL) ON COMMIT DROP;
INSERT INTO outbound_copy (connection, copy)
SELECT id, gen_random_uuid() FROM public.git_host_connection WHERE receives_events AND reports_status;

ALTER TABLE public.git_host_connection ADD COLUMN kind smallint DEFAULT 0 NOT NULL;
UPDATE public.git_host_connection SET kind = CASE WHEN receives_events THEN 0 ELSE 1 END;
DROP INDEX public.git_host_connection_github_installation_id;
DROP INDEX public.git_host_connection_instance_name;
DROP INDEX public.git_host_connection_team_name;
DROP INDEX public.git_host_connection_project_name;
ALTER TABLE public.git_host_connection DROP CONSTRAINT git_host_connection_one_owner;
CREATE UNIQUE INDEX "idx-integration-project-kind-name" ON public.git_host_connection USING btree (project, kind, name);
INSERT INTO public.git_host_connection
    (id, project, name, display_name, kind, git_host_type, base_url, access_token, created_by,
     created_at, managed, receives_events, reports_status)
SELECT o.copy, c.project, c.name, c.display_name, 1, c.git_host_type, c.base_url, c.access_token,
    c.created_by, c.created_at, c.managed, false, true
FROM outbound_copy o JOIN public.git_host_connection c ON c.id = o.connection;

INSERT INTO public.task_action
    (id, task, name, action_type, config, events, active, created_by, created_at, updated_at)
SELECT gen_random_uuid(), t.id, 'git-host-status', 2,
    jsonb_build_object('type', 'git_host_status_report', 'integration_id',
        coalesce(o.copy, t.git_host_connection)),
    '[]'::jsonb, true, t.created_by, now(), now()
FROM public.task t
LEFT JOIN outbound_copy o ON o.connection = t.git_host_connection
WHERE t.report_status AND t.git_host_connection IS NOT NULL
ON CONFLICT (task, name) DO NOTHING;

UPDATE public.task_trigger tt
SET config = tt.config || jsonb_build_object('integration_id', t.git_host_connection)
FROM public.task t
WHERE t.id = tt.task AND tt.trigger_type IN (1, 2) AND t.git_host_connection IS NOT NULL;

DROP TABLE public.task_commit_status_post_attempt;
DROP INDEX public.idx_task_git_host_connection;
ALTER TABLE public.task DROP COLUMN report_status, DROP COLUMN git_host_connection;

ALTER INDEX public.incoming_webhook_delivery_git_host_connection_created_at
    RENAME TO incoming_webhook_delivery_integration_created_at;
ALTER TABLE public.incoming_webhook_delivery RENAME COLUMN git_host_connection TO integration;
ALTER INDEX public."idx-exec_job_pull_request-git_host_connection" RENAME TO "idx-exec_job_pull_request-integration";
ALTER TABLE public.exec_job_pull_request
    RENAME CONSTRAINT "fk-exec_job_pull_request-git_host_connection" TO "fk-exec_job_pull_request-integration";
ALTER TABLE public.exec_job_pull_request RENAME COLUMN git_host_connection TO integration;


CREATE TABLE public.github_installation (
    id uuid NOT NULL,
    project uuid NOT NULL,
    installation_id bigint NOT NULL,
    account_login text,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL
);
ALTER TABLE ONLY public.github_installation
    ADD CONSTRAINT github_installation_pkey PRIMARY KEY (id);
CREATE UNIQUE INDEX "idx-github-installation-project-installation" ON public.github_installation USING btree (project, installation_id);
ALTER TABLE ONLY public.github_installation
    ADD CONSTRAINT "fk-github_installation-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id);
ALTER TABLE ONLY public.github_installation
    ADD CONSTRAINT "fk-github_installation-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;
ALTER TABLE public.git_host_connection ADD COLUMN github_installation uuid;
ALTER TABLE ONLY public.git_host_connection
    ADD CONSTRAINT "fk-integration-github-installation" FOREIGN KEY (github_installation) REFERENCES public.github_installation(id) ON DELETE SET NULL;

ALTER TABLE public.git_host_connection
    DROP COLUMN account_login,
    DROP COLUMN github_installation_id,
    DROP COLUMN reports_status,
    DROP COLUMN receives_events,
    DROP COLUMN owner_team;
ALTER TABLE public.git_host_connection ALTER COLUMN project SET NOT NULL;
ALTER TABLE public.git_host_connection RENAME COLUMN base_url TO endpoint_url;
ALTER TABLE public.git_host_connection RENAME CONSTRAINT "fk-git_host_connection-project" TO "fk-integration-project";
ALTER TABLE public.git_host_connection RENAME CONSTRAINT "fk-git_host_connection-created_by" TO "fk-integration-created_by";
ALTER TABLE public.git_host_connection RENAME CONSTRAINT git_host_connection_pkey TO integration_pkey;
ALTER TABLE public.git_host_connection RENAME TO integration;
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
