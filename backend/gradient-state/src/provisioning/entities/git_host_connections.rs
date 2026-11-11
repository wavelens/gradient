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
use gradient_db::git_host_connections::{self, GitHostConnectionOwnerByDatabaseId};
use gradient_types::git_host::GitHostConnectionOwner;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_git_host_connections(
        &self,
        state_connections: &HashMap<String, StateGitHostConnection>,
        team_ids: &HashMap<String, TeamId>,
    ) -> Result<(), DynError> {
        if state_connections.is_empty() {
            return Ok(());
        }

        let project_map = self.project_lookup().await?;
        let user_map = self.user_lookup().await?;

        for (label, state_connection) in state_connections {
            let owner = state_connection
                .owner
                .parse::<GitHostConnectionOwner>()
                .map_err(|e| {
                    format!(
                        "Invalid owner for connection '{}' to the Git host: {}",
                        label, e
                    )
                })?;
            let owner_id = match &owner {
                GitHostConnectionOwner::Instance => {
                    Some(GitHostConnectionOwnerByDatabaseId::Instance)
                }
                GitHostConnectionOwner::Team { name } => team_ids
                    .get(name)
                    .copied()
                    .map(GitHostConnectionOwnerByDatabaseId::Team),
                GitHostConnectionOwner::Project { name } => project_map
                    .get(name)
                    .copied()
                    .map(GitHostConnectionOwnerByDatabaseId::Project),
            }
            .ok_or_else(|| {
                format!(
                    "Git host connection '{}' references an unknown owner '{}'",
                    label, owner
                )
            })?;
            let (project, owner_team) = match owner_id {
                GitHostConnectionOwnerByDatabaseId::Instance => (None, None),
                GitHostConnectionOwnerByDatabaseId::Team(team) => (None, Some(team)),
                GitHostConnectionOwnerByDatabaseId::Project(project) => (Some(project), None),
            };

            let created_by_id = lookup_id(&user_map, &state_connection.created_by, "User")?;

            let git_host = GitHostType::from_path_segment(&state_connection.git_host_type).ok_or_else(|| {
                format!(
                    "Git host connection '{}' has invalid git_host_type '{}'. Expected gitea/forgejo/gitlab/github.",
                    label, state_connection.git_host_type
                )
            })?;

            let github_installation_id = if matches!(git_host, GitHostType::GitHub) {
                Some(
                    state_connection
                        .installation_id
                        .filter(|id| *id > 0)
                        .ok_or_else(|| {
                            format!(
                                "Git host connection '{}' has git_host_type 'github' but no positive installation_id",
                                label
                            )
                        })?,
                )
            } else {
                None
            };

            let encrypted_secret = self.read_and_encrypt_git_host_connection_credential_file(
                state_connection.secret_file.as_deref(),
                label,
                "secret",
            )?;
            let encrypted_token = self.read_and_encrypt_git_host_connection_credential_file(
                state_connection.access_token_file.as_deref(),
                label,
                "token",
            )?;

            let base_url = state_connection
                .base_url
                .as_deref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let display_name = state_connection
                .display_name
                .clone()
                .unwrap_or_else(|| state_connection.name.clone());

            let existing =
                match git_host_connections::by_name(self.db, owner_id, &state_connection.name)
                    .await?
                {
                    Some(existing) => Some(existing),
                    None => match github_installation_id {
                        Some(id) => {
                            git_host_connections::by_github_installation_id(self.db, id).await?
                        }
                        None => None,
                    },
                };

            if let Some(existing) = existing {
                let mut active = existing.into_active_model();
                active.project = Set(project);
                active.owner_team = Set(owner_team);
                active.name = Set(state_connection.name.clone());
                active.display_name = Set(display_name);
                active.git_host_type = Set(git_host);
                active.base_url = Set(base_url);
                active.secret = Set(encrypted_secret);
                active.access_token = Set(encrypted_token);
                active.receives_events = Set(state_connection.receives_events);
                active.reports_status = Set(state_connection.reports_status);
                active.github_installation_id = Set(github_installation_id);
                active.account_login = Set(state_connection.account_login.clone());
                active.created_by = Set(created_by_id);
                active.managed = Set(true);
                active.update(self.db).await?;
                tracing::info!(%label, "Updated the managed connection to the Git host.");
            } else {
                MGitHostConnection {
                    id: GitHostConnectionId::now_v7(),
                    project,
                    owner_team,
                    name: state_connection.name.clone(),
                    display_name,
                    git_host_type: git_host,
                    secret: encrypted_secret,
                    base_url,
                    access_token: encrypted_token,
                    allowed_ips: None,
                    receives_events: state_connection.receives_events,
                    reports_status: state_connection.reports_status,
                    github_installation_id,
                    account_login: state_connection.account_login.clone(),
                    created_by: created_by_id,
                    created_at: now(),
                    managed: true,
                }
                .into_active_model()
                .insert(self.db)
                .await?;
                tracing::info!(%label, "Created a managed connection to a Git host.");
            }
        }

        Ok(())
    }

    pub(crate) fn read_and_encrypt_git_host_connection_credential_file(
        &self,
        field_set: Option<&str>,
        label: &str,
        suffix: &str,
    ) -> Result<Option<String>, DynError> {
        if field_set.is_none() {
            return Ok(None);
        }
        let (plain, _) = read_credential(
            self.credentials_dir,
            "git_host_connection",
            label,
            suffix,
            &format!("git host connection {} file", suffix),
        )?;
        let encrypted =
            encrypt_secret_with_file(self.crypt_secret_file, plain.trim()).map_err(|e| {
                format!(
                    "Failed to encrypt {} for the connection to Git host '{}': {}",
                    suffix, label, e
                )
            })?;
        Ok(Some(encrypted))
    }
}
