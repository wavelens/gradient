/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
};

fn on_branch(task: TaskId, branch: &str) -> Condition {
    Condition::all()
        .add(CEvaluation::Task.eq(task))
        .add(CEvaluation::Branch.eq(branch))
}

fn active() -> Condition {
    let codes: Vec<i32> = EvaluationStatus::ACTIVE
        .iter()
        .copied()
        .map(i32::from)
        .collect();
    Condition::all().add(CEvaluation::Status.is_in(codes))
}

/// Mirrors `uq_evaluation_one_active_per_branch`: active and not concurrent.
pub async fn in_flight_on_branch<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    branch: &str,
) -> Result<Option<MEvaluation>, DbErr> {
    EEvaluation::find()
        .filter(on_branch(task, branch))
        .filter(active())
        .filter(CEvaluation::Concurrent.eq(false))
        .one(db)
        .await
}

pub async fn newest_on_branch<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    branch: &str,
) -> Result<Option<MEvaluation>, DbErr> {
    EEvaluation::find()
        .filter(on_branch(task, branch))
        .order_by_desc(CEvaluation::CreatedAt)
        .order_by_desc(CEvaluation::Id)
        .one(db)
        .await
}

pub async fn active_on_branch<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    branch: &str,
) -> Result<Vec<MEvaluation>, DbErr> {
    EEvaluation::find()
        .filter(on_branch(task, branch))
        .filter(active())
        .all(db)
        .await
}
