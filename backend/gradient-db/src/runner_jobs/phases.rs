/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::runner::Phase;
use gradient_types::{
    CRunnerJobAttempt, CRunnerJobPhase, ERunnerJob, ERunnerJobAttempt, ERunnerJobPhase, MRunnerJob,
    MRunnerJobPhase, RunnerJobAttemptId, RunnerJobId, RunnerJobPhaseId,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub async fn record<C: ConnectionTrait>(
    db: &C,
    attempt: RunnerJobAttemptId,
    name: String,
    payload: Vec<u8>,
) -> Result<Option<MRunnerJob>, DbErr> {
    let Some(open) = ERunnerJobAttempt::find_by_id(attempt)
        .filter(CRunnerJobAttempt::EndedAt.is_null())
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    crate::rows::insert::<ERunnerJobPhase, _>(
        db,
        MRunnerJobPhase {
            id: RunnerJobPhaseId::now_v7(),
            runner_job: open.runner_job,
            attempt,
            name,
            payload,
            created_at: gradient_types::now(),
        },
    )
    .await?;
    ERunnerJob::find_by_id(open.runner_job).one(db).await
}

pub async fn latest<C: ConnectionTrait>(db: &C, job: RunnerJobId) -> Result<Option<Phase>, DbErr> {
    Ok(ERunnerJobPhase::find()
        .filter(CRunnerJobPhase::RunnerJob.eq(job))
        .order_by_desc(CRunnerJobPhase::CreatedAt)
        .one(db)
        .await?
        .map(|phase| Phase {
            name: phase.name,
            payload: phase.payload,
        }))
}
