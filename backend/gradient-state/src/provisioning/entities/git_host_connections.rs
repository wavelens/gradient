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
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
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

            let declared_id = state_connection
                .id
                .as_deref()
                .map(|id| {
                    id.trim().parse::<GitHostConnectionId>().map_err(|e| {
                        format!(
                            "Git host connection '{}' has an invalid id '{}': {}",
                            label, id, e
                        )
                    })
                })
                .transpose()?;

            let existing = self
                .find_existing_git_host_connection_row(
                    label,
                    declared_id,
                    owner_id,
                    &state_connection.name,
                    github_installation_id,
                )
                .await?;

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
                active.encrypted_refresh_token = Set(None);
                active.access_token_expires_at = Set(None);
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
                    id: declared_id.unwrap_or_else(GitHostConnectionId::now_v7),
                    project,
                    owner_team,
                    name: state_connection.name.clone(),
                    display_name,
                    git_host_type: git_host,
                    secret: encrypted_secret,
                    base_url,
                    access_token: encrypted_token,
                    encrypted_refresh_token: None,
                    access_token_expires_at: None,
                    allowed_ips: None,
                    receives_events: state_connection.receives_events,
                    reports_status: state_connection.reports_status,
                    github_installation_id,
                    account_login: state_connection.account_login.clone(),
                    created_by: created_by_id,
                    created_at: now(),
                    managed: true,
                    refused_signature_count: 0,
                    last_refused_signature_at: None,
                }
                .into_active_model()
                .insert(self.db)
                .await?;
                tracing::info!(%label, "Created a managed connection to a Git host.");
            }
        }

        Ok(())
    }

    async fn find_existing_git_host_connection_row(
        &self,
        label: &str,
        declared_id: Option<GitHostConnectionId>,
        owner_id: GitHostConnectionOwnerByDatabaseId,
        name: &str,
        github_installation_id: Option<i64>,
    ) -> Result<Option<MGitHostConnection>, DynError> {
        let pinned = match declared_id {
            Some(id) => EGitHostConnection::find_by_id(id).one(self.db).await?,
            None => None,
        };

        let existing = match git_host_connections::by_name(self.db, owner_id, name).await? {
            Some(existing) => Some(existing),
            None => match github_installation_id {
                Some(id) => git_host_connections::by_github_installation_id(self.db, id).await?,
                None => None,
            },
        };

        if let Some(pinned) = pinned {
            if let Some(other) = existing.filter(|other| other.id != pinned.id) {
                return Err(format!(
                    "The pinned ID for Git host connection \"{}\" is {}. That name or the ID of the GitHub installation is already in use by connection {}.",
                    label, pinned.id, other.id
                )
                .into());
            }
            return Ok(Some(pinned));
        }

        if let (Some(declared), Some(row)) = (declared_id, &existing) {
            return Err(format!(
                "Git host connection '{}' with ID {} already present. The declared ID in state is {}. IDs are immutable.",
                label, row.id, declared
            )
            .into());
        }

        Ok(existing)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use sea_orm::DatabaseConnection;
    use std::path::Path;

    fn state_connection(name: &str, id: Option<GitHostConnectionId>) -> StateGitHostConnection {
        StateGitHostConnection {
            id: id.map(|id| id.to_string()),
            name: name.into(),
            display_name: None,
            owner: "instance".into(),
            git_host_type: "gitea".into(),
            secret_file: None,
            base_url: Some("https://forge.example.com".into()),
            access_token_file: None,
            receives_events: true,
            reports_status: false,
            installation_id: None,
            account_login: None,
            created_by: rows::user().username,
        }
    }

    async fn database_with_user() -> TestDatabase {
        let db = TestDatabase::create().await;
        rows::user()
            .into_active_model()
            .insert(&db.connection())
            .await
            .unwrap();
        db
    }

    async fn apply(
        db: &DatabaseConnection,
        connection: StateGitHostConnection,
    ) -> Result<(), DynError> {
        let app = StateApplicator {
            db,
            crypt_secret_file: "",
            credentials_dir: Path::new(""),
            email_enabled: false,
        };
        app.apply_git_host_connections(
            &HashMap::from([(connection.name.clone(), connection)]),
            &HashMap::new(),
        )
        .await
    }

    #[tokio::test]
    async fn applying_state_twice_renaming_pinned_git_host_connection_keeps_row_and_id() {
        let db = database_with_user().await;
        let pinned = GitHostConnectionId::now_v7();

        apply(&db.connection(), state_connection("forge", Some(pinned)))
            .await
            .unwrap();
        apply(&db.connection(), state_connection("renamed", Some(pinned)))
            .await
            .unwrap();

        let rows = db.rows::<EGitHostConnection>().await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, pinned);
        assert_eq!(rows[0].name, "renamed");
        db.finish().await;
    }

    #[tokio::test]
    async fn applying_state_again_keeps_the_refused_signature_count_and_time() {
        let db = database_with_user().await;
        apply(&db.connection(), state_connection("forge", None))
            .await
            .unwrap();
        let refused_at = gradient_fixtures::test_date();
        let mut row = db
            .rows::<EGitHostConnection>()
            .await
            .remove(0)
            .into_active_model();
        row.refused_signature_count = Set(3);
        row.last_refused_signature_at = Set(Some(refused_at));
        row.update(&db.connection()).await.unwrap();

        apply(&db.connection(), state_connection("forge", None))
            .await
            .unwrap();

        let row = db.rows::<EGitHostConnection>().await.remove(0);
        assert_eq!(row.refused_signature_count, 3);
        assert_eq!(row.last_refused_signature_at, Some(refused_at));
        db.finish().await;
    }

    #[tokio::test]
    async fn apply_git_host_connection_with_mismatched_id_fails_and_leaves_existing_row_unchanged()
    {
        let db = database_with_user().await;
        apply(&db.connection(), state_connection("forge", None))
            .await
            .unwrap();
        let existing = db.rows::<EGitHostConnection>().await.remove(0);

        let declared = GitHostConnectionId::now_v7();
        let err = apply(&db.connection(), state_connection("forge", Some(declared)))
            .await
            .unwrap_err();

        assert!(err.to_string().contains(&declared.to_string()), "{err}");
        let rows = db.rows::<EGitHostConnection>().await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, existing.id);
        db.finish().await;
    }

    #[tokio::test]
    async fn rename_pinned_git_host_connection_to_existing_name_fails_naming_both_ids_and_leaves_rows_unchanged()
     {
        let db = database_with_user().await;
        let pinned = GitHostConnectionId::now_v7();
        apply(&db.connection(), state_connection("forge", Some(pinned)))
            .await
            .unwrap();
        apply(&db.connection(), state_connection("taken", None))
            .await
            .unwrap();
        let before = db.rows::<EGitHostConnection>().await;
        let taken = before.iter().find(|row| row.name == "taken").unwrap().id;

        let err = apply(&db.connection(), state_connection("taken", Some(pinned)))
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains(&pinned.to_string()), "{err}");
        assert!(err.contains(&taken.to_string()), "{err}");
        let mut after = db.rows::<EGitHostConnection>().await;
        after.sort_by_key(|row| row.id);
        let mut before = before;
        before.sort_by_key(|row| row.id);
        let names = |rows: &[MGitHostConnection]| {
            rows.iter()
                .map(|row| (row.id, row.name.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&after), names(&before));
        db.finish().await;
    }
}
