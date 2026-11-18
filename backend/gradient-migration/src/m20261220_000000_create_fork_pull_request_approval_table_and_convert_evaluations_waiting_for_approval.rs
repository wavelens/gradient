/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
CREATE TABLE public.fork_pull_request_approval (
    id uuid NOT NULL,
    task uuid NOT NULL,
    trigger uuid NOT NULL,
    pr_number bigint NOT NULL,
    pr_author text NOT NULL,
    commit_hash bytea NOT NULL,
    head_repo_clone_url text,
    commit_message text,
    author_name text,
    check_run_id bigint,
    approving_maintainer_login text,
    approved_at timestamp without time zone,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT fork_pull_request_approval_pkey PRIMARY KEY (id),
    CONSTRAINT fork_pull_request_approval_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE,
    CONSTRAINT fork_pull_request_approval_trigger_fkey FOREIGN KEY (trigger) REFERENCES public.task_trigger(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX "uq-fork_pull_request_approval-task-pr_number-commit_hash"
    ON public.fork_pull_request_approval USING btree (task, pr_number, commit_hash);
CREATE INDEX "idx-fork_pull_request_approval-trigger"
    ON public.fork_pull_request_approval USING btree (trigger);
CREATE INDEX "idx-fork_pull_request_approval-check_run_id"
    ON public.fork_pull_request_approval USING btree (check_run_id)
    WHERE check_run_id IS NOT NULL;

INSERT INTO public.fork_pull_request_approval (
    id, task, trigger, pr_number, pr_author, commit_hash, head_repo_clone_url,
    commit_message, author_name, check_run_id, created_at
)
SELECT DISTINCT ON (e.task, (e.waiting_reason->>'pr_number')::bigint, c.hash)
    gen_random_uuid(),
    e.task,
    e.trigger,
    (e.waiting_reason->>'pr_number')::bigint,
    COALESCE(e.waiting_reason->>'pr_author', ''),
    c.hash,
    NULLIF(e.repository, t.repository),
    c.message,
    c.author_name,
    (e.check_run_ids->>('gradient/' || t.name || ': Approval'))::bigint,
    e.created_at
FROM public.evaluation AS e
JOIN public.task AS t ON t.id = e.task
JOIN public.commit AS c ON c.id = e.commit
WHERE e.status = 4
  AND e.waiting_reason->>'kind' = 'approval'
  AND e.trigger IS NOT NULL
ORDER BY e.task, (e.waiting_reason->>'pr_number')::bigint, c.hash, e.created_at DESC;

UPDATE public.evaluation
SET status = 7,
    abort_reason = 1,
    waiting_reason = NULL,
    finished_at = now() AT TIME ZONE 'UTC',
    updated_at = now() AT TIME ZONE 'UTC'
WHERE status = 4
  AND waiting_reason->>'kind' = 'approval';
"#;

const DOWN: &str = r#"
DROP TABLE public.fork_pull_request_approval;
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
