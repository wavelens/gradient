/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub(super) async fn snapshot_flake_input_overrides<C: ConnectionTrait>(
    txn: &C,
    project_id: gradient_entity::ids::ProjectId,
    evaluation_id: gradient_entity::ids::EvaluationId,
) -> Result<(), sea_orm::DbErr> {
    let rows = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Project.eq(project_id))
        .all(txn)
        .await?;

    for r in rows {
        let am = AEvaluationFlakeInputOverride {
            id: Set(EvaluationFlakeInputOverrideId::now_v7()),
            evaluation: Set(evaluation_id),
            input_name: Set(r.input_name),
            url: Set(r.url),
        };
        am.insert(txn).await?;
    }
    Ok(())
}
