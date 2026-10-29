/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::{ExecJobId, MExecJob};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Value};
use std::sync::LazyLock;

use crate::sql::status::exec_in;

static SET_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET keep_vm_running = $2 \
         WHERE entry_point = (SELECT entry_point FROM exec_job WHERE id = $1) \
         AND status IN ({open}) RETURNING *",
        open = exec_in(&ExecJobStatus::OPEN),
    )
});

crate::sql_lazy! {
    SET_EXEC_JOB_KEEP_VM_RUNNING = || SET_SQL.as_str(),
        params = [NewUuid, Bool(true)];
}

pub async fn set<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    keep: bool,
) -> Result<Vec<MExecJob>, DbErr> {
    MExecJob::find_by_statement(
        SET_EXEC_JOB_KEEP_VM_RUNNING
            .bind([Value::Uuid(Some(job.into_inner())), Value::Bool(Some(keep))]),
    )
    .all(db)
    .await
}
