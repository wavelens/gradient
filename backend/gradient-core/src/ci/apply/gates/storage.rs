/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::db::org_caches_all_full;
use crate::types::waiting_reason::WaitingReason;
use crate::types::*;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ConnectionTrait};

/// Move a freshly-created `Queued` evaluation into `Waiting` with
/// `WaitingReason::CacheStorageFull` when every writable cache for the org is
/// within `STORAGE_HEADROOM_BYTES` of its configured `max_storage_gb` (or the
/// instance-wide limit). Returns the evaluation unchanged when at least one
/// writable cache still has headroom, or when the org has no writable cache at
/// all (that case is owned by [`park_if_no_cache`](super::park_if_no_cache)).
pub async fn park_if_storage_full<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    organization: OrganizationId,
    instance_max_storage_gb: i32,
) -> Result<MEvaluation, sea_orm::DbErr> {
    if eval.status != EvaluationStatus::Queued {
        return Ok(eval);
    }
    if !org_caches_all_full(db, organization, instance_max_storage_gb).await? {
        return Ok(eval);
    }
    let mut ae: AEvaluation = eval.into();
    ae.status = Set(EvaluationStatus::Waiting);
    ae.waiting_reason = Set(Some(WaitingReason::CacheStorageFull.to_json()));
    ae.updated_at = Set(crate::types::now());
    ae.update(db).await
}
