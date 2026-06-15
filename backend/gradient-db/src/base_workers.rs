/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Base-worker lookups: identity by worker_id, per-org enablement, and the
//! enabled-org set used to scope a connecting base worker.

use gradient_types::ids::{BaseWorkerId, OrganizationId};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

/// Returns the enabled `base_worker` row for this worker_id, if any.
pub async fn enabled_base_worker_by_worker_id<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Option<gradient_entity::base_worker::Model>, sea_orm::DbErr> {
    use gradient_entity::base_worker::{Column as C2, Entity as E};

    E::find()
        .filter(C2::WorkerId.eq(worker_id))
        .filter(C2::Enabled.eq(true))
        .one(db)
        .await
}

/// Org UUIDs that have opted into the given base worker.
pub async fn orgs_enabling_base_worker<C: ConnectionTrait>(
    db: &C,
    base_worker: BaseWorkerId,
) -> Result<Vec<OrganizationId>, sea_orm::DbErr> {
    use gradient_entity::organization_base_worker::{Column as C2, Entity as E};

    Ok(E::find()
        .filter(C2::BaseWorker.eq(base_worker))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.organization)
        .collect())
}

/// True when the org has an enabled base worker with the `eval` gate on.
pub async fn org_has_eval_capable_base_worker<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<bool, sea_orm::DbErr> {
    use gradient_entity::base_worker::{Column as BWC, Entity as BW};
    use gradient_entity::organization_base_worker::{Column as OBWC, Entity as OBW};

    let enabled_ids: Vec<BaseWorkerId> = OBW::find()
        .filter(OBWC::Organization.eq(organization))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.base_worker)
        .collect();

    if enabled_ids.is_empty() {
        return Ok(false);
    }

    let row = BW::find()
        .filter(BWC::Id.is_in(enabled_ids))
        .filter(BWC::Enabled.eq(true))
        .filter(BWC::EnableEval.eq(true))
        .one(db)
        .await?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn orgs_enabling_returns_empty_when_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::organization_base_worker::Model>::new()])
            .into_connection();
        let out = orgs_enabling_base_worker(&db, BaseWorkerId::nil()).await.unwrap();
        assert!(out.is_empty());
    }
}
