/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::{
    AExecJobOutput, CExecJob, CExecJobOutput, EExecJob, EExecJobOutput, EvaluationId, ExecJobId,
    MExecJobOutput,
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect,
};

pub async fn upsert<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    path: &str,
    size: i64,
    content_type: &str,
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    let row: AExecJobOutput = MExecJobOutput {
        exec_job: job,
        path: path.to_owned(),
        size,
        content_type: content_type.to_owned(),
        created_at: now,
        updated_at: now,
    }
    .into_active_model();
    EExecJobOutput::insert(row)
        .on_conflict(
            OnConflict::columns([CExecJobOutput::ExecJob, CExecJobOutput::Path])
                .update_columns([
                    CExecJobOutput::Size,
                    CExecJobOutput::ContentType,
                    CExecJobOutput::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn of_job<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
) -> Result<Vec<MExecJobOutput>, DbErr> {
    EExecJobOutput::find()
        .filter(CExecJobOutput::ExecJob.eq(job))
        .order_by_asc(CExecJobOutput::Path)
        .all(db)
        .await
}

pub async fn one<C: ConnectionTrait>(
    db: &C,
    job: ExecJobId,
    path: &str,
) -> Result<Option<MExecJobOutput>, DbErr> {
    EExecJobOutput::find_by_id((job, path.to_owned()))
        .one(db)
        .await
}

pub async fn jobs_of_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<Vec<ExecJobId>, DbErr> {
    crate::fetch_in_chunks(evaluations, |chunk| async move {
        EExecJob::find()
            .select_only()
            .column(CExecJob::Id)
            .filter(CExecJob::Evaluation.is_in(chunk))
            .into_tuple()
            .all(db)
            .await
    })
    .await
}
