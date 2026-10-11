/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, IntoActiveModel};

pub async fn insert_many<C: ConnectionTrait>(
    db: &C,
    overrides: Vec<MEvaluationFlakeInputOverride>,
) -> Result<(), DbErr> {
    let rows: Vec<AEvaluationFlakeInputOverride> = overrides
        .into_iter()
        .map(IntoActiveModel::into_active_model)
        .collect();
    EEvaluationFlakeInputOverride::insert_many(rows)
        .exec(db)
        .await?;

    Ok(())
}
