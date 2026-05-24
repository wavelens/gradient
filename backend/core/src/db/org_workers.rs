/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Organisation ↔ worker registration helpers consumed by the trigger
//! pipeline (no-workers gate) and the worker-register reconcile path.

use crate::types::ids::OrganizationId;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

/// Returns `true` when the organisation has at least one active worker
/// registration with the `eval` capability gate enabled.
///
/// A `Queued` evaluation can only progress once an `eval`-capable worker
/// picks up its `FlakeJob`. Until then the build dispatch reconciler has
/// nothing to do - there are no builds yet - so an org without any
/// eval-capable registration would otherwise sit in `Queued` forever.
pub async fn org_has_eval_capable_worker_registration<C: ConnectionTrait>(
    db: &C,
    organization: OrganizationId,
) -> Result<bool, sea_orm::DbErr> {
    use entity::worker_registration::{Column as CWR, Entity as EWR};

    let row = EWR::find()
        .filter(CWR::PeerId.eq(organization))
        .filter(CWR::Active.eq(true))
        .filter(CWR::EnableEval.eq(true))
        .one(db)
        .await?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::{UserId, WorkerRegistrationId};
    use chrono::NaiveDateTime;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn registration_row(active: bool, enable_eval: bool) -> entity::worker_registration::Model {
        entity::worker_registration::Model {
            id: WorkerRegistrationId::now_v7(),
            peer_id: OrganizationId::nil(),
            worker_id: "00000000-0000-4000-8000-000000000001".into(),
            token_hash: String::new(),
            managed: false,
            url: None,
            active,
            enable_fetch: true,
            enable_eval,
            enable_build: true,
            display_name: String::new(),
            created_by: Some(UserId::nil()),
            created_at: NaiveDateTime::default(),
        }
    }

    #[tokio::test]
    async fn returns_true_when_eval_capable_registration_exists() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![registration_row(true, true)]])
            .into_connection();

        let out = org_has_eval_capable_worker_registration(&db, OrganizationId::nil())
            .await
            .unwrap();
        assert!(out);
    }

    #[tokio::test]
    async fn returns_false_when_no_registrations_exist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<entity::worker_registration::Model>::new()])
            .into_connection();

        let out = org_has_eval_capable_worker_registration(&db, OrganizationId::nil())
            .await
            .unwrap();
        assert!(!out);
    }
}
