/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{lookup_id, read_credential};
use crate::config::*;
use anyhow::Result;
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_entity::*;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_workers(
        &self,
        state_workers: &HashMap<String, StateWorker>,
        team_ids: &HashMap<String, TeamId>,
    ) -> Result<(), DynError> {
        let project_map = self.project_lookup().await?;
        let user_map = self.user_lookup().await?;

        for state_worker in state_workers.values() {
            let (token, _) = read_credential(
                "worker",
                &state_worker.worker_id,
                "token",
                "worker token file",
            )?;
            let token_hash = password_auth::generate_hash(token.trim());
            let token_encrypted = self.encrypt_dial_token(state_worker, token.trim())?;
            let created_by_id = state_worker
                .created_by
                .as_ref()
                .map(|user| lookup_id(&user_map, user, "User"))
                .transpose()?;

            if let Some(team) = &state_worker.team {
                let team_id = lookup_id(team_ids, team, "Team")?;
                apply_team_worker(
                    self.db,
                    state_worker,
                    team_id,
                    created_by_id,
                    token_hash,
                    token_encrypted,
                )
                .await?;
                continue;
            }

            let url = state_worker
                .url
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            for project_name in &state_worker.projects {
                let peer_id = lookup_id(&project_map, project_name, "Project")?;

                let existing = worker_registration::Entity::find()
                    .filter(worker_registration::Column::PeerId.eq(peer_id))
                    .filter(worker_registration::Column::WorkerId.eq(&state_worker.worker_id))
                    .one(self.db)
                    .await?;

                if let Some(existing) = existing {
                    let mut reg: worker_registration::ActiveModel = existing.into();
                    reg.token_hash = Set(token_hash.clone());
                    reg.token_encrypted = Set(token_encrypted.clone());
                    reg.managed = Set(true);
                    reg.url = Set(url.clone());
                    reg.display_name = Set(state_worker.display_name.clone());
                    reg.active = Set(state_worker.enabled);
                    reg.enable_fetch = Set(state_worker.enable_fetch);
                    reg.enable_eval = Set(state_worker.enable_eval);
                    reg.enable_build = Set(state_worker.enable_build);
                    reg.created_by = Set(created_by_id);
                    reg.update(self.db).await?;
                    tracing::info!(
                        worker_id = %state_worker.worker_id,
                        project = %project_name,
                        "Updated worker registration"
                    );
                } else {
                    let reg = worker_registration::Model {
                        id: WorkerRegistrationId::now_v7(),
                        peer_id,
                        worker_id: state_worker.worker_id.clone(),
                        token_hash: token_hash.clone(),
                        token_encrypted: token_encrypted.clone(),
                        managed: true,
                        url: url.clone(),
                        display_name: state_worker.display_name.clone(),
                        gradient_ci: false,
                        active: state_worker.enabled,
                        enable_fetch: state_worker.enable_fetch,
                        enable_eval: state_worker.enable_eval,
                        enable_build: state_worker.enable_build,
                        created_by: created_by_id,
                        created_at: now(),
                    }
                    .into_active_model();

                    reg.insert(self.db).await?;
                    tracing::info!(
                        worker_id = %state_worker.worker_id,
                        project = %project_name,
                        "Created worker registration"
                    );
                }
            }
        }

        Ok(())
    }

    fn encrypt_dial_token(
        &self,
        worker: &StateWorker,
        token: &str,
    ) -> Result<Option<String>, DynError> {
        let dialed = worker.url.as_deref().is_some_and(|u| !u.trim().is_empty());
        if !dialed {
            return Ok(None);
        }

        encrypt_secret_with_file(self.crypt_secret_file, token)
            .map(Some)
            .map_err(|e| {
                format!(
                    "Failed to encrypt token for worker '{}': {e}",
                    worker.worker_id
                )
                .into()
            })
    }
}

async fn apply_team_worker<C: ConnectionTrait>(
    db: &C,
    worker: &StateWorker,
    team: TeamId,
    created_by: Option<UserId>,
    token_hash: String,
    token_encrypted: Option<String>,
) -> Result<(), DynError> {
    let existing = team_worker::Entity::find()
        .filter(team_worker::Column::WorkerId.eq(worker.worker_id.clone()))
        .one(db)
        .await?;

    if let Some(row) = existing {
        let mut active: team_worker::ActiveModel = row.into();
        active.team = Set(team);
        active.token_hash = Set(token_hash);
        active.token_encrypted = Set(token_encrypted);
        active.url = Set(worker.url.clone());
        active.display_name = Set(worker.display_name.clone());
        active.enable_fetch = Set(worker.enable_fetch);
        active.enable_eval = Set(worker.enable_eval);
        active.enable_build = Set(worker.enable_build);
        active.active = Set(worker.enabled);
        active.managed = Set(true);
        active.update(db).await?;
        tracing::info!(worker_id = %worker.worker_id, "Updated team worker");
        return Ok(());
    }

    team_worker::Model {
        id: TeamWorkerId::now_v7(),
        team,
        worker_id: worker.worker_id.clone(),
        token_hash,
        token_encrypted,
        url: worker.url.clone(),
        display_name: worker.display_name.clone(),
        gradient_ci: false,
        enable_fetch: worker.enable_fetch,
        enable_eval: worker.enable_eval,
        enable_build: worker.enable_build,
        active: worker.enabled,
        managed: true,
        created_by,
        created_at: now(),
    }
    .into_active_model()
    .insert(db)
    .await?;
    tracing::info!(worker_id = %worker.worker_id, "Created team worker");
    Ok(())
}

#[cfg(test)]
mod team_worker_tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn a_declared_team_worker_is_inserted_as_managed() {
        let team = TeamId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<team_worker::Model>::new()])
            .append_query_results([vec![team_worker::Model {
                team,
                worker_id: "w1".into(),
                ..Default::default()
            }]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let worker = StateWorker {
            worker_id: "w1".into(),
            url: None,
            projects: vec![],
            team: Some("platform".into()),
            token_file: "/dev/null".into(),
            display_name: "W".into(),
            created_by: None,
            enable_fetch: true,
            enable_eval: true,
            enable_build: true,
            enabled: true,
        };

        apply_team_worker(&db, &worker, team, None, "hash".into(), None)
            .await
            .unwrap();

        let inserted = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .find(|s| s.sql.starts_with("INSERT INTO \"team_worker\""))
            .expect("an INSERT INTO team_worker");
        assert!(inserted.sql.contains("\"managed\""));
    }
}
