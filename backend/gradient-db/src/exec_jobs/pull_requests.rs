/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::{
    CExecJobPullRequest, EExecJobPullRequest, ExecJobId, ExecJobPullRequestId, IntegrationId,
    MExecJobPullRequest,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub async fn record<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    integration: IntegrationId,
    url: &str,
    branch: &str,
    number: i64,
) -> Result<(), DbErr> {
    crate::rows::insert::<EExecJobPullRequest, _>(
        db,
        MExecJobPullRequest {
            id: ExecJobPullRequestId::now_v7(),
            exec_job: job,
            integration,
            url: url.to_owned(),
            branch: branch.to_owned(),
            number,
            created_at: gradient_types::now(),
        },
    )
    .await?;
    Ok(())
}

pub async fn of_job<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
) -> Result<Vec<MExecJobPullRequest>, DbErr> {
    EExecJobPullRequest::find()
        .filter(CExecJobPullRequest::ExecJob.eq(job))
        .order_by_desc(CExecJobPullRequest::CreatedAt)
        .all(db)
        .await
}
