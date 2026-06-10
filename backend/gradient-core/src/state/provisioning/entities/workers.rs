/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{lookup_id, read_credential};
use crate::state::config::*;
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
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
                    let reg = worker_registration::ActiveModel {
                        id: Set(WorkerRegistrationId::now_v7()),
                        peer_id: Set(peer_id),
                        worker_id: Set(state_worker.worker_id.clone()),
                        token_hash: Set(token_hash.clone()),
                        managed: Set(true),
                        url: Set(url.clone()),
                        display_name: Set(state_worker.display_name.clone()),
                        active: Set(true),
                        enable_fetch: Set(state_worker.enable_fetch),
                        enable_eval: Set(state_worker.enable_eval),
                        enable_build: Set(state_worker.enable_build),
                        created_by: Set(Some(created_by_id)),
                        created_at: Set(now()),
                    };
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
