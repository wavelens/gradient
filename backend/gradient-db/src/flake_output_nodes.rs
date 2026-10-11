/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::flake_output_node::{Column, Entity, Model};
use gradient_entity::ids::EvaluationId;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .filter(Column::Evaluation.eq(evaluation))
        .all(db)
        .await
}
