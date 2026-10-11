/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, Order, QueryFilter, QueryOrder};

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<MEvaluationMessage>, DbErr> {
    EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation))
        .all(db)
        .await
}

pub async fn of_evaluation_oldest_first<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<MEvaluationMessage>, DbErr> {
    EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation))
        .order_by(CEvaluationMessage::CreatedAt, Order::Asc)
        .all(db)
        .await
}

pub async fn entry_points_of_messages<C: ConnectionTrait>(
    db: &C,
    messages: Vec<EvaluationMessageId>,
) -> Result<Vec<MEntryPointMessage>, DbErr> {
    EEntryPointMessage::find()
        .filter(CEntryPointMessage::Message.is_in(messages))
        .all(db)
        .await
}
