/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD helpers for the `admin_task` table.

use anyhow::{Context, Result};
use chrono::{NaiveDateTime, SubsecRound};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, QuerySelect,
};
use serde_json::Value as JsonValue;

use gradient_entity::ids::{AdminTaskId, UserId};
use gradient_types::*;

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
    let model = MAdminTask {
        id: AdminTaskId::now_v7(),
        kind,
        status: AdminTaskStatus::Pending,
        created_at: now(),
        created_by,
        ..Default::default()
    }
    .into_active_model();

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
    use sea_orm::sea_query::ExprTrait;

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

/// Move a pending task to running and return the `started_at` that names this
/// run in [`save_checkpoint`] and [`complete`]. `None` when it is not pending.
pub async fn start<C: ConnectionTrait>(conn: &C, id: AdminTaskId) -> Result<Option<NaiveDateTime>> {
    let started_at = now().trunc_subsecs(6);
    let res = EAdminTask::update_many()
        .col_expr(CAdminTask::Status, Expr::value(AdminTaskStatus::Running))
        .col_expr(CAdminTask::StartedAt, Expr::value(started_at))
        .filter(CAdminTask::Id.eq(id))
        .filter(CAdminTask::Status.eq(AdminTaskStatus::Pending))
        .exec(conn)
        .await
        .context("start admin_task")?;
    Ok((res.rows_affected == 1).then_some(started_at))
}

/// Record the last finished unit of the run begun at `started_at`. `false` when
/// [`restart`] or an end of the task took it away from this run meanwhile.
pub async fn save_checkpoint<C: ConnectionTrait>(
    conn: &C,
    id: AdminTaskId,
    started_at: NaiveDateTime,
    checkpoint: &str,
    progress: JsonValue,
) -> Result<bool> {
    let res = EAdminTask::update_many()
        .col_expr(CAdminTask::Checkpoint, Expr::value(checkpoint))
        .col_expr(CAdminTask::Progress, Expr::value(progress))
        .filter(CAdminTask::Id.eq(id))
        .filter(CAdminTask::Status.eq(AdminTaskStatus::Running))
        .filter(CAdminTask::StartedAt.eq(started_at))
        .exec(conn)
        .await
        .context("save admin_task checkpoint")?;
    Ok(res.rows_affected == 1)
}

/// Complete the run begun at `started_at`, unless it was restarted meanwhile.
pub async fn complete<C: ConnectionTrait>(
    conn: &C,
    id: AdminTaskId,
    started_at: NaiveDateTime,
    progress: JsonValue,
) -> Result<bool> {
    let res = EAdminTask::update_many()
        .col_expr(CAdminTask::Status, Expr::value(AdminTaskStatus::Completed))
        .col_expr(CAdminTask::Progress, Expr::value(progress))
        .col_expr(CAdminTask::FinishedAt, Expr::value(now()))
        .filter(CAdminTask::Id.eq(id))
        .filter(CAdminTask::Status.eq(AdminTaskStatus::Running))
        .filter(CAdminTask::StartedAt.eq(started_at))
        .exec(conn)
        .await
        .context("complete admin_task")?;
    Ok(res.rows_affected == 1)
}

/// Send an active task back to pending with no checkpoint, owned by
/// `created_by`, so the next run starts over from its first unit.
pub async fn restart<C: ConnectionTrait>(
    conn: &C,
    id: AdminTaskId,
    created_by: Option<UserId>,
) -> Result<bool> {
    let res = EAdminTask::update_many()
        .col_expr(CAdminTask::Status, Expr::value(AdminTaskStatus::Pending))
        .col_expr(CAdminTask::StartedAt, Expr::value(Option::<NaiveDateTime>::None))
        .col_expr(CAdminTask::Checkpoint, Expr::value(Option::<String>::None))
        .col_expr(CAdminTask::Progress, Expr::value(Option::<JsonValue>::None))
        .col_expr(CAdminTask::CreatedBy, Expr::value(created_by))
        .filter(CAdminTask::Id.eq(id))
        .filter(CAdminTask::Status.is_in([AdminTaskStatus::Pending, AdminTaskStatus::Running]))
        .exec(conn)
        .await
        .context("restart admin_task")?;
    Ok(res.rows_affected == 1)
}

/// When the last task of `kind` finished, if any did.
pub async fn last_finished_at<C: ConnectionTrait>(
    conn: &C,
    kind: AdminTaskKind,
) -> Result<Option<NaiveDateTime>> {
    Ok(EAdminTask::find()
        .select_only()
        .column(CAdminTask::FinishedAt)
        .filter(CAdminTask::Kind.eq(kind))
        .filter(CAdminTask::FinishedAt.is_not_null())
        .order_by_desc(CAdminTask::FinishedAt)
        .into_tuple::<Option<NaiveDateTime>>()
        .one(conn)
        .await
        .context("last finished admin_task")?
        .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_violation_detection_is_case_insensitive() {
        assert!(is_unique_violation("ERROR: duplicate key value"));
        assert!(is_unique_violation("violates UNIQUE constraint"));
        assert!(!is_unique_violation("connection refused"));
    }
}
