/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD helpers for the `admin_task` table.

use anyhow::{Context, Result};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Statement,
};
use serde_json::Value as JsonValue;

use crate::types::*;
use gradient_entity::ids::{AdminTaskId, UserId};

#[derive(Debug)]
pub enum InsertPendingError {
    AlreadyActive(AdminTaskId),
    Db(anyhow::Error),
}

pub async fn insert_pending<C: ConnectionTrait>(
    conn: &C,
    kind: AdminTaskKind,
    created_by: Option<UserId>,
) -> std::result::Result<MAdminTask, InsertPendingError> {
    let model = AAdminTask {
        id: Set(AdminTaskId::now_v7()),
        kind: Set(kind),
        status: Set(AdminTaskStatus::Pending),
        created_at: Set(now()),
        started_at: Set(None),
        finished_at: Set(None),
        progress: Set(None),
        error: Set(None),
        created_by: Set(created_by),
    };
    match model.insert(conn).await {
        Ok(m) => Ok(m),
        Err(DbErr::Exec(e)) if is_unique_violation(&e.to_string()) => {
            let active = find_active(conn, kind)
                .await
                .map_err(InsertPendingError::Db)?
                .ok_or_else(|| {
                    InsertPendingError::Db(anyhow::anyhow!(
                        "unique violation but no active row found"
                    ))
                })?;
            Err(InsertPendingError::AlreadyActive(active.id))
        }
        Err(DbErr::Query(e)) if is_unique_violation(&e.to_string()) => {
            let active = find_active(conn, kind)
                .await
                .map_err(InsertPendingError::Db)?
                .ok_or_else(|| {
                    InsertPendingError::Db(anyhow::anyhow!(
                        "unique violation but no active row found"
                    ))
                })?;
            Err(InsertPendingError::AlreadyActive(active.id))
        }
        Err(other) => Err(InsertPendingError::Db(
            anyhow::Error::from(other).context("insert admin_task"),
        )),
    }
}

fn is_unique_violation(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("unique") || m.contains("duplicate key")
}

pub async fn find_active<C: ConnectionTrait>(
    conn: &C,
    kind: AdminTaskKind,
) -> Result<Option<MAdminTask>> {
    EAdminTask::find()
        .filter(CAdminTask::Kind.eq(kind))
        .filter(
            CAdminTask::Status
                .eq(AdminTaskStatus::Pending)
                .or(CAdminTask::Status.eq(AdminTaskStatus::Running)),
        )
        .one(conn)
        .await
        .context("find active admin_task")
}

pub async fn list_recent<C: ConnectionTrait>(conn: &C, limit: u64) -> Result<Vec<MAdminTask>> {
    EAdminTask::find()
        .order_by_desc(CAdminTask::CreatedAt)
        .limit(limit)
        .all(conn)
        .await
        .context("list recent admin_task")
}

pub async fn get<C: ConnectionTrait>(conn: &C, id: AdminTaskId) -> Result<Option<MAdminTask>> {
    EAdminTask::find_by_id(id)
        .one(conn)
        .await
        .context("get admin_task")
}

pub async fn mark_running<C: ConnectionTrait>(conn: &C, id: AdminTaskId) -> Result<()> {
    let Some(model) = get(conn, id).await? else {
        return Ok(());
    };
    let mut a: AAdminTask = model.into_active_model();
    a.status = Set(AdminTaskStatus::Running);
    a.started_at = Set(Some(now()));
    a.update(conn).await.context("mark_running")?;
    Ok(())
}

pub async fn update_progress<C: ConnectionTrait>(
    conn: &C,
    id: AdminTaskId,
    progress: JsonValue,
) -> Result<()> {
    let Some(model) = get(conn, id).await? else {
        return Ok(());
    };
    let mut a: AAdminTask = model.into_active_model();
    a.progress = Set(Some(progress));
    a.update(conn).await.context("update_progress")?;
    Ok(())
}

pub async fn mark_completed<C: ConnectionTrait>(
    conn: &C,
    id: AdminTaskId,
    progress: JsonValue,
) -> Result<()> {
    let Some(model) = get(conn, id).await? else {
        return Ok(());
    };
    let mut a: AAdminTask = model.into_active_model();
    a.status = Set(AdminTaskStatus::Completed);
    a.progress = Set(Some(progress));
    a.finished_at = Set(Some(now()));
    a.update(conn).await.context("mark_completed")?;
    Ok(())
}

pub async fn mark_failed<C: ConnectionTrait>(
    conn: &C,
    id: AdminTaskId,
    error: String,
    progress: Option<JsonValue>,
) -> Result<()> {
    let Some(model) = get(conn, id).await? else {
        return Ok(());
    };
    let mut a: AAdminTask = model.into_active_model();
    a.status = Set(AdminTaskStatus::Failed);
    a.error = Set(Some(error));
    if let Some(p) = progress {
        a.progress = Set(Some(p));
    }
    a.finished_at = Set(Some(now()));
    a.update(conn).await.context("mark_failed")?;
    Ok(())
}

pub const STARTUP_FAILURE_MESSAGE: &str = "server restarted before completion";

pub async fn mark_all_active_failed<C: ConnectionTrait>(conn: &C) -> Result<u64> {
    let res = conn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"UPDATE admin_task
               SET status = $1,
                   error = COALESCE(error, $2),
                   finished_at = NOW() AT TIME ZONE 'UTC'
               WHERE status IN ($3, $4)"#,
            [
                sea_orm::Value::Int(Some(AdminTaskStatus::Failed as i32)),
                sea_orm::Value::String(Some(Box::new(STARTUP_FAILURE_MESSAGE.into()))),
                sea_orm::Value::Int(Some(AdminTaskStatus::Pending as i32)),
                sea_orm::Value::Int(Some(AdminTaskStatus::Running as i32)),
            ],
        ))
        .await
        .context("mark_all_active_failed")?;
    Ok(res.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn task_row(id: AdminTaskId, status: AdminTaskStatus) -> MAdminTask {
        MAdminTask {
            id,
            kind: AdminTaskKind::DeepGc,
            status,
            created_at: now(),
            started_at: None,
            finished_at: None,
            progress: None,
            error: None,
            created_by: None,
        }
    }

    #[tokio::test]
    async fn insert_pending_returns_inserted_row() {
        let id = AdminTaskId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![task_row(id, AdminTaskStatus::Pending)]])
            .into_connection();
        let inserted = insert_pending(&db, AdminTaskKind::DeepGc, None)
            .await
            .unwrap();
        assert_eq!(inserted.status, AdminTaskStatus::Pending);
    }

    #[tokio::test]
    async fn find_active_filters_by_kind_and_status() {
        let id = AdminTaskId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![task_row(id, AdminTaskStatus::Running)]])
            .into_connection();
        let got = find_active(&db, AdminTaskKind::DeepGc).await.unwrap();
        assert_eq!(got.map(|r| r.id), Some(id));
    }

    #[tokio::test]
    async fn mark_running_sets_status_and_started_at() {
        let id = AdminTaskId::now_v7();
        let row = task_row(id, AdminTaskStatus::Pending);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row.clone()]])
            .append_query_results([vec![row]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        mark_running(&db, id).await.unwrap();
    }

    #[tokio::test]
    async fn mark_all_active_failed_executes_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 7,
            }])
            .into_connection();
        assert_eq!(mark_all_active_failed(&db).await.unwrap(), 7);
    }

    #[test]
    fn unique_violation_detection_is_case_insensitive() {
        assert!(is_unique_violation("ERROR: duplicate key value"));
        assert!(is_unique_violation("violates UNIQUE constraint"));
        assert!(!is_unique_violation("connection refused"));
    }
}
