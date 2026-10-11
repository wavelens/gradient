/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build_log_chunk::{Column, Entity, Model};
use gradient_types::ids::BuildAttemptId;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub async fn of_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: BuildAttemptId,
) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .filter(Column::BuildAttempt.eq(attempt))
        .order_by_asc(Column::ChunkIndex)
        .all(db)
        .await
}

pub async fn by_index<C: ConnectionTrait>(
    db: &C,
    attempt: BuildAttemptId,
    index: i32,
) -> Result<Option<Model>, DbErr> {
    Entity::find()
        .filter(Column::BuildAttempt.eq(attempt))
        .filter(Column::ChunkIndex.eq(index))
        .one(db)
        .await
}
