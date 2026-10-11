/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Evaluation.is_in(evaluations.iter().copied()))
        .all(db)
        .await
}

pub async fn of_derivations_in_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
    derivations: &[DerivationId],
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Evaluation.is_in(evaluations.iter().copied()))
        .filter(CBuildJob::Derivation.is_in(derivations.iter().copied()))
        .all(db)
        .await
}

pub async fn of_derivations_in_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> Result<Vec<MBuildJob>, DbErr> {
    crate::fetch_in_chunks(derivations, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Evaluation.eq(evaluation))
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
}
