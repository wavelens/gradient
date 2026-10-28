/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::{
    EExecJobToken, ExecJobAttemptId, ExecJobId, ExecJobTokenId, MExecJobToken, ProjectId, TaskId,
    UserId,
};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Value};

#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct ResolvedJobToken {
    pub job: ExecJobId,
    pub attempt: ExecJobAttemptId,
    pub project: ProjectId,
    pub task: TaskId,
    pub project_name: String,
    pub task_name: String,
    pub acting_user: UserId,
}

#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct JobTokenRow {
    pub attempt_number: i32,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub revoked: bool,
}

crate::sql! {
    RESOLVE_JOB_TOKEN = "SELECT a.exec_job AS job, t.attempt, t.project, t.task, \
         p.name AS project_name, tk.name AS task_name, \
         COALESCE(ev.started_by, tk.created_by) AS acting_user \
         FROM exec_job_token t \
         JOIN exec_job_attempt a ON a.id = t.attempt \
         JOIN exec_job j ON j.id = a.exec_job \
         JOIN evaluation ev ON ev.id = j.evaluation \
         JOIN task tk ON tk.id = t.task \
         JOIN project p ON p.id = t.project \
         WHERE t.token_hash = $1 AND NOT t.revoked AND t.expires_at > $2",
        params = [Text("0000"), Now];
    JOB_TOKENS_OF_JOB = "SELECT a.number AS attempt_number, t.created_at, t.expires_at, t.revoked \
         FROM exec_job_token t JOIN exec_job_attempt a ON a.id = t.attempt \
         WHERE a.exec_job = $1 ORDER BY a.number, t.created_at",
        params = [NewUuid];
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    token_hash: &str,
    project: ProjectId,
    task: TaskId,
    expires_at: NaiveDateTime,
) -> Result<(), DbErr> {
    crate::rows::insert::<EExecJobToken, _>(
        db,
        MExecJobToken {
            id: ExecJobTokenId::now_v7(),
            attempt,
            token_hash: token_hash.to_owned(),
            project,
            task,
            expires_at,
            revoked: false,
            created_at: gradient_types::now(),
        },
    )
    .await?;
    Ok(())
}

pub async fn resolve<C: ConnectionTrait>(
    db: &C,
    token_hash: &str,
    now: NaiveDateTime,
) -> Result<Option<ResolvedJobToken>, DbErr> {
    ResolvedJobToken::find_by_statement(RESOLVE_JOB_TOKEN.bind([
        Value::String(Some(token_hash.to_owned())),
        Value::ChronoDateTime(Some(now)),
    ]))
    .one(db)
    .await
}

pub async fn of_job<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<Vec<JobTokenRow>, DbErr> {
    JobTokenRow::find_by_statement(JOB_TOKENS_OF_JOB.bind([Value::Uuid(Some(job.into_inner()))]))
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};

    #[tokio::test]
    async fn a_token_resolves_only_unrevoked_and_unexpired() {
        let db = mock_db()
            .append_query_results([Vec::<MExecJobToken>::new()])
            .into_connection();

        let resolved = resolve(&db, "hash", gradient_types::now()).await.unwrap();

        assert!(resolved.is_none());
        let log = all_statements(db.into_transaction_log());
        assert!(
            log[0].sql.contains("NOT t.revoked") && log[0].sql.contains("t.expires_at > $2"),
            "{}",
            log[0].sql
        );
    }
}
