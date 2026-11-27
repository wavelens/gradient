/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::{ExecJobAttemptId, ExecJobId, ExecJobTokenId, ProjectId, TaskId, UserId};
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
    pub allows_project_credentials_and_privileged_actions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct JobTokenRow {
    pub attempt_number: i32,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub revoked: bool,
}

crate::sql! {
    INSERT_EXEC_JOB_TOKEN = "INSERT INTO exec_job_token \
         (id, attempt, token_hash, project, task, expires_at, revoked, created_at) \
         SELECT $1::uuid, a.id, $3::text, tk.project, tk.id, $4::timestamp, false, \
              (now() AT TIME ZONE 'UTC') \
         FROM exec_job_attempt a \
         JOIN exec_job j ON j.id = a.exec_job \
         JOIN evaluation ev ON ev.id = j.evaluation \
         JOIN task tk ON tk.id = ev.task \
         WHERE a.id = $2",
        params = [NewUuid, NewUuid, Text("0000"), Now];
    RESOLVE_JOB_TOKEN = "SELECT a.exec_job AS job, t.attempt, t.project, t.task, \
         p.name AS project_name, tk.name AS task_name, \
         COALESCE(ev.started_by, tk.created_by) AS acting_user, \
         ev.allows_project_credentials_and_privileged_actions \
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
    expires_at: NaiveDateTime,
) -> Result<bool, DbErr> {
    let inserted = db
        .execute_raw(INSERT_EXEC_JOB_TOKEN.bind([
            Value::Uuid(Some(ExecJobTokenId::now_v7().into_inner())),
            Value::Uuid(Some(attempt.into_inner())),
            Value::String(Some(token_hash.to_owned())),
            Value::ChronoDateTime(Some(expires_at)),
        ]))
        .await?;
    Ok(inserted.rows_affected() == 1)
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
    use super::super::attempts::{open, tests::two_queued_jobs};
    use super::*;
    use gradient_types::DispatchedJobId;

    #[tokio::test]
    async fn job_token_resolves_only_while_unrevoked_and_unexpired() {
        let (db, job, _) = two_queued_jobs().await;
        let conn = db.connection();
        let attempt = ExecJobAttemptId::now_v7();
        open(
            &conn,
            job,
            attempt,
            1,
            DispatchedJobId::now_v7(),
            "w1",
            2,
            None,
        )
        .await
        .unwrap();
        let now = gradient_types::now();
        for (hash, expires_at) in [
            ("live", now + chrono::Duration::hours(1)),
            ("revoked", now + chrono::Duration::hours(1)),
            ("expired", now - chrono::Duration::hours(1)),
        ] {
            assert!(insert(&conn, attempt, hash, expires_at).await.unwrap());
        }
        db.execute("UPDATE exec_job_token SET revoked = true WHERE token_hash = 'revoked'")
            .await;

        let live = resolve(&conn, "live", now).await.unwrap();
        let revoked = resolve(&conn, "revoked", now).await.unwrap();
        let expired = resolve(&conn, "expired", now).await.unwrap();
        let unknown = resolve(&conn, "unknown", now).await.unwrap();
        let listed = of_job(&conn, job).await.unwrap();

        assert_eq!(live.map(|t| (t.job, t.attempt)), Some((job, attempt)));
        assert!(revoked.is_none() && expired.is_none() && unknown.is_none());
        assert_eq!(listed.len(), 3);
        assert_eq!(listed.iter().filter(|t| t.revoked).count(), 1);
        db.finish().await;
    }
}
