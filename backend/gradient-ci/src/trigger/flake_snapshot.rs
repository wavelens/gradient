/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
};

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
        let am = MEvaluationFlakeInputOverride {
            id: EvaluationFlakeInputOverrideId::now_v7(),
            evaluation: evaluation_id,
            input_name: r.input_name,
            url: r.url,
        }
        .into_active_model();

        am.insert(txn).await?;
    }
    Ok(())
}
