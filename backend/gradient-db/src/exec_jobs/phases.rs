/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::exec::Phase;
use gradient_types::{
    CExecJobAttempt, CExecJobPhase, EExecJob, EExecJobAttempt, EExecJobPhase, ExecJobAttemptId,
    ExecJobId, ExecJobPhaseId, MExecJob, MExecJobPhase,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub async fn record<C: ConnectionTrait>(
    db: &C,
    attempt: ExecJobAttemptId,
    name: String,
    payload: Vec<u8>,
) -> Result<Option<MExecJob>, DbErr> {
    let Some(open) = EExecJobAttempt::find_by_id(attempt)
        .filter(CExecJobAttempt::EndedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    crate::rows::insert::<EExecJobPhase, _>(
        db,
        MExecJobPhase {
            id: ExecJobPhaseId::now_v7(),
            exec_job: open.exec_job,
            attempt,
            name,
            payload,
            created_at: gradient_types::now(),
        },
    )
    .await?;
    EExecJob::find_by_id(open.exec_job).one(db).await
}

pub async fn latest<C: ConnectionTrait>(db: &C, job: ExecJobId) -> Result<Option<Phase>, DbErr> {
    Ok(EExecJobPhase::find()
        .filter(CExecJobPhase::ExecJob.eq(job))
        .order_by_desc(CExecJobPhase::CreatedAt)
        .order_by_desc(CExecJobPhase::Id)
        .one(db)
        .await?
        .map(|phase| Phase {
            name: phase.name,
            payload: phase.payload,
        }))
}
