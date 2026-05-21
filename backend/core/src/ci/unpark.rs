/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Side-effect helpers that flip parked evaluations back to `Queued` once
//! the external condition they were waiting on clears.
//!
//! `NoCache` parks: triggered when the project's organisation had no writable
//! cache subscription. Caller (`orgs/settings.rs::subscribe_cache`) invokes
//! [`unpark_no_cache_for_org`] right after inserting the subscription row;
//! the caller is also responsible for re-emitting the `Pending` CI status
//! for each unparked evaluation.

use crate::types::ids::OrganizationId;
use crate::types::waiting_reason::WaitingReason;
use crate::types::*;

use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

/// Flip every evaluation parked with `WaitingReason::NoCache` for projects in
/// `organization` back to `Queued`. Returns the updated rows so the caller
/// can re-emit pending CI checks.
pub async fn unpark_no_cache_for_org<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    let project_ids: Vec<ProjectId> = EProject::find()
        .filter(CProject::Organization.eq(organization))
        .all(db)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();

    if project_ids.is_empty() {
        return Ok(Vec::new());
    }

    let parked = EEvaluation::find()
        .filter(CEvaluation::Project.is_in(project_ids))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(db)
        .await?;

    let candidates: Vec<MEvaluation> = parked
        .into_iter()
        .filter(|e| {
            e.waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json)
                .is_some_and(|r| matches!(r, WaitingReason::NoCache))
        })
        .collect();

    let mut unparked = Vec::with_capacity(candidates.len());
    for eval in candidates {
        let mut ae: AEvaluation = eval.into();
        ae.status = Set(EvaluationStatus::Queued);
        ae.waiting_reason = Set(None);
        ae.updated_at = Set(crate::types::now());
        unparked.push(ae.update(db).await?);
    }
    Ok(unparked)
}
