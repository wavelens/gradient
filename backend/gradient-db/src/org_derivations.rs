/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Derivations an organization has built. Replaces the dropped per-org
//! `derivation.organization` scoping now that derivations are a global graph:
//! ownership is derived through the org's projects -> evaluations -> builds.

use crate::fetch_in_chunks;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;

/// Distinct derivations referenced by builds in `org_id`'s evaluations.
pub async fn derivation_ids_for_org<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<Vec<DerivationId>, DbErr> {
    let project_ids: Vec<ProjectId> = EProject::find()
        .select_only()
        .column(CProject::Id)
        .filter(CProject::Organization.eq(org_id))
        .into_tuple::<ProjectId>()
        .all(db)
        .await?;
    if project_ids.is_empty() {
        return Ok(vec![]);
    }

    let eval_ids = fetch_in_chunks(&project_ids, |chunk| async move {
        EEvaluation::find()
            .select_only()
            .column(CEvaluation::Id)
            .filter(CEvaluation::Project.is_in(chunk))
            .into_tuple::<EvaluationId>()
            .all(db)
            .await
    })
    .await?;
    if eval_ids.is_empty() {
        return Ok(vec![]);
    }

    let drv_ids = fetch_in_chunks(&eval_ids, |chunk| async move {
        EBuild::find()
            .select_only()
            .column(CBuild::Derivation)
            .distinct()
            .filter(CBuild::Evaluation.is_in(chunk))
            .into_tuple::<DerivationId>()
            .all(db)
            .await
    })
    .await?;

    Ok(drv_ids.into_iter().collect::<HashSet<_>>().into_iter().collect())
}
