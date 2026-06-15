/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{lookup_id, read_credential};
use crate::config::*;
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    // ── apply_workers ─────────────────────────────────────────────────────────

    pub(crate) async fn apply_workers(
        &self,
        state_workers: &HashMap<String, StateWorker>,
    ) -> Result<(), DynError> {
        let org_map = self.org_lookup().await?;
        let user_map = self.user_lookup().await?;

        for state_worker in state_workers.values() {
            let (token, _) = read_credential(
                "worker",
                &state_worker.worker_id,
                "token",
                "worker token file",
            )?;
            let token_hash = password_auth::generate_hash(token.trim());
            let created_by_id = lookup_id(&user_map, &state_worker.created_by, "User")?;

            if state_worker.base_worker {
                apply_base_worker(self.db, state_worker, &org_map, created_by_id, token_hash)
                    .await?;

                continue;
            }

            let url = state_worker
                .url
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            for org_name in &state_worker.organizations {
                let peer_id = lookup_id(&org_map, org_name, "Organization")?;

                let existing = worker_registration::Entity::find()
                    .filter(worker_registration::Column::PeerId.eq(peer_id))
                    .filter(worker_registration::Column::WorkerId.eq(&state_worker.worker_id))
                    .one(self.db)
                    .await?;

                if let Some(existing) = existing {
                    let mut reg: worker_registration::ActiveModel = existing.into();
                    reg.token_hash = Set(token_hash.clone());
                    reg.managed = Set(true);
                    reg.url = Set(url.clone());
                    reg.display_name = Set(state_worker.display_name.clone());
                    reg.enable_fetch = Set(state_worker.enable_fetch);
                    reg.enable_eval = Set(state_worker.enable_eval);
                    reg.enable_build = Set(state_worker.enable_build);
                    reg.created_by = Set(Some(created_by_id));
                    reg.update(self.db).await?;
                    tracing::info!(
                        worker_id = %state_worker.worker_id,
                        organization = %org_name,
                        "Updated worker registration"
                    );
                } else {
                    let reg = worker_registration::Model {
                        id: WorkerRegistrationId::now_v7(),
                        peer_id,
                        worker_id: state_worker.worker_id.clone(),
                        token_hash: token_hash.clone(),
                        managed: true,
                        url: url.clone(),
                        display_name: state_worker.display_name.clone(),
                        active: true,
                        enable_fetch: state_worker.enable_fetch,
                        enable_eval: state_worker.enable_eval,
                        enable_build: state_worker.enable_build,
                        created_by: Some(created_by_id),
                        created_at: now(),
                    }
                    .into_active_model();

                    reg.insert(self.db).await?;
                    tracing::info!(
                        worker_id = %state_worker.worker_id,
                        organization = %org_name,
                        "Created worker registration"
                    );
                }
            }
        }

        Ok(())
    }
}

// ── base workers ──────────────────────────────────────────────────────────

/// Upserts the server-level `base_worker` row, then pre-enables its declared
/// orgs. Pre-enablements are only added: frontend opt-ins are not state-managed
/// and must survive reconciliation.
async fn apply_base_worker<C: ConnectionTrait>(
    db: &C,
    worker: &StateWorker,
    org_map: &HashMap<String, OrganizationId>,
    user_id: UserId,
    token_hash: String,
) -> Result<(), DynError> {
    let authorize_against = worker
        .authorize_against
        .as_ref()
        .map(|s| s.parse::<uuid::Uuid>())
        .transpose()?;

    let existing = base_worker::Entity::find()
        .filter(base_worker::Column::WorkerId.eq(worker.worker_id.clone()))
        .one(db)
        .await?;

    let base_worker_id = if let Some(row) = existing {
        let id = row.id;
        let mut am: base_worker::ActiveModel = row.into();
        am.token_hash = Set(token_hash);
        am.url = Set(worker.url.clone());
        am.display_name = Set(worker.display_name.clone());
        am.enable_fetch = Set(worker.enable_fetch);
        am.enable_eval = Set(worker.enable_eval);
        am.enable_build = Set(worker.enable_build);
        am.enabled = Set(worker.enabled);
        am.authorize_against = Set(authorize_against);
        am.update(db).await?;
        tracing::info!(worker_id = %worker.worker_id, "Updated base worker");
        id
    } else {
        let id = BaseWorkerId::now_v7();
        base_worker::Model {
            id,
            worker_id: worker.worker_id.clone(),
            token_hash,
            url: worker.url.clone(),
            display_name: worker.display_name.clone(),
            enable_fetch: worker.enable_fetch,
            enable_eval: worker.enable_eval,
            enable_build: worker.enable_build,
            enabled: worker.enabled,
            authorize_against,
            created_by: Some(user_id),
            created_at: now(),
        }
        .into_active_model()
        .insert(db)
        .await?;
        tracing::info!(worker_id = %worker.worker_id, "Created base worker");
        id
    };

    reconcile_pre_enabled_orgs(db, base_worker_id, worker, org_map, user_id).await
}

async fn reconcile_pre_enabled_orgs<C: ConnectionTrait>(
    db: &C,
    base_worker_id: BaseWorkerId,
    worker: &StateWorker,
    org_map: &HashMap<String, OrganizationId>,
    user_id: UserId,
) -> Result<(), DynError> {
    let existing = organization_base_worker::Entity::find()
        .filter(organization_base_worker::Column::BaseWorker.eq(base_worker_id))
        .all(db)
        .await?;

    for org_name in &worker.organizations {
        let org_id = lookup_id(org_map, org_name, "Organization")?;
        if existing.iter().any(|r| r.organization == org_id) {
            continue;
        }

        organization_base_worker::Model {
            id: OrganizationBaseWorkerId::now_v7(),
            organization: org_id,
            base_worker: base_worker_id,
            created_by: Some(user_id),
            created_at: now(),
        }
        .into_active_model()
        .insert(db)
        .await?;
        tracing::info!(
            worker_id = %worker.worker_id,
            organization = %org_name,
            "Pre-enabled base worker for organization"
        );
    }

    Ok(())
}

#[cfg(test)]
mod base_worker_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn base_worker() -> StateWorker {
        StateWorker {
            worker_id: "bw-1".to_string(),
            url: Some("https://bw.example".to_string()),
            organizations: vec![],
            token_file: "/dev/null".to_string(),
            display_name: "Base".to_string(),
            created_by: "alice".to_string(),
            enable_fetch: true,
            enable_eval: true,
            enable_build: true,
            base_worker: true,
            authorize_against: None,
            enabled: true,
        }
    }

    /// With no existing row, `apply_base_worker` must INSERT into `base_worker`.
    #[tokio::test]
    async fn apply_base_worker_inserts_when_absent() {
        let inserted = base_worker::Model {
            worker_id: "bw-1".to_string(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<base_worker::Model>::new()])
            .append_query_results([vec![inserted]])
            .append_query_results([Vec::<organization_base_worker::Model>::new()])
            .into_connection();

        apply_base_worker(
            &db,
            &base_worker(),
            &HashMap::new(),
            UserId::now_v7(),
            "hash".to_string(),
        )
        .await
        .unwrap();

        let logs = db.into_transaction_log();
        let insert = logs
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| s.sql.to_lowercase().contains("insert into \"base_worker\""))
            .expect("expected an INSERT INTO base_worker statement");

        assert!(insert.sql.to_lowercase().contains("\"worker_id\""));
    }
}
