/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::StateApplicator;
use super::super::{DynError, lookup_id, read_credential};
use crate::config::{StateHostLoginEntry, StateProject};
use crate::validation::invalid_git_host_error_message;
use gradient_db::project_git_https_credentials as credentials;
use gradient_types::https_git_url_host::normalize_bare_https_git_host;
use gradient_types::{MProjectGitHttpsCredential, ProjectId};
use std::collections::{HashMap, HashSet};

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_project_git_https_credentials(
        &self,
        projects: &HashMap<String, StateProject>,
    ) -> Result<(), DynError> {
        let project_map = self.project_lookup().await?;

        for project in projects.values() {
            let project_id = lookup_id(&project_map, &project.name, "Project")?;
            self.apply_https_git_credentials_of_project(project_id, project)
                .await?;
        }

        Ok(())
    }

    async fn apply_https_git_credentials_of_project(
        &self,
        project_id: ProjectId,
        project: &StateProject,
    ) -> Result<(), DynError> {
        let stored = credentials::list(self.db, project_id).await?;
        let mut declared_hosts = HashSet::new();

        for (index, declared) in project.git_https_credentials.iter().enumerate() {
            let host = normalize_bare_https_git_host(&declared.host)
                .ok_or_else(|| invalid_git_host_error_message(&declared.host))?;
            let password =
                self.read_https_git_credential_password_at_index(project, index, &host)?;
            let unchanged = stored
                .iter()
                .find(|row| row.host == host)
                .is_some_and(|row| {
                    stored_credential_row_matches_declared_credential(
                        self.crypt_secret_file,
                        row,
                        declared,
                        &password,
                    )
                });
            if !unchanged {
                let encrypted =
                    gradient_sources::encrypt_secret(self.crypt_secret_file, &password)?;
                credentials::upsert(
                    self.db,
                    project_id,
                    &host,
                    &declared.username,
                    encrypted,
                    None,
                )
                .await?;
            }
            declared_hosts.insert(host);
        }

        let mut removed = 0usize;
        for row in stored
            .iter()
            .filter(|row| !declared_hosts.contains(&row.host))
        {
            if credentials::delete(self.db, project_id, row.id).await? {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(project = %project.name, removed, "Removed HTTPS credentials for Git no longer in state");
        }

        Ok(())
    }

    fn read_https_git_credential_password_at_index(
        &self,
        project: &StateProject,
        index: usize,
        host: &str,
    ) -> Result<String, DynError> {
        let (contents, _) = read_credential(
            self.credentials_dir,
            "project",
            &project.name,
            &format!("git_https_{index}_password"),
            &format!("Password file for git over HTTPS on host '{host}'"),
        )?;
        let password = contents.trim();
        if password.is_empty() {
            return Err(format!(
                "Project '{}' has an empty password file for HTTPS git on host '{}'.",
                project.name, host
            )
            .into());
        }
        Ok(password.to_owned())
    }
}

fn stored_credential_row_matches_declared_credential(
    crypt_secret_file: &str,
    row: &MProjectGitHttpsCredential,
    declared: &StateHostLoginEntry,
    password: &str,
) -> bool {
    row.username == declared.username
        && gradient_sources::decrypt_secret(crypt_secret_file, &row.password)
            .is_ok_and(|stored| stored.expose() == password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use sea_orm::{ActiveModelTrait, DatabaseConnection, IntoActiveModel};
    use std::path::Path;

    const CRYPT_SECRET: &str = "this-is-a-32-byte-crypt-key!!!!";

    fn state_project(name: &str, hosts: &[&str]) -> StateProject {
        StateProject {
            owner_team: None,
            name: name.into(),
            display_name: name.into(),
            id: None,
            description: None,
            private_key_file: String::new(),
            secret_files: Default::default(),
            git_https_credentials: hosts
                .iter()
                .map(|host| StateHostLoginEntry {
                    host: host.to_string(),
                    username: "ci".into(),
                    password_file: format!("/run/secrets/{host}"),
                })
                .collect(),
            public: false,
            hide_build_requests: false,
            allow_exec_in_build_requests: false,
            created_by: "alice".into(),
            members: vec![],
            teams: vec![],
        }
    }

    fn credentials_dir_with_passwords(project: &str, passwords: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("crypt"), CRYPT_SECRET).unwrap();
        for (index, password) in passwords.iter().enumerate() {
            std::fs::write(
                dir.path().join(format!(
                    "gradient_project_{project}_git_https_{index}_password"
                )),
                format!("{password}\n"),
            )
            .unwrap();
        }
        dir
    }

    async fn database_with_project() -> (TestDatabase, String, ProjectId) {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let project = rows::project()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        (db, project.name, project.id)
    }

    async fn apply(db: &DatabaseConnection, dir: &Path, project: StateProject) {
        let crypt_file = dir.join("crypt");
        let app = StateApplicator {
            db,
            crypt_secret_file: crypt_file.to_str().unwrap(),
            credentials_dir: dir,
            email_enabled: false,
        };
        app.apply_project_git_https_credentials(&HashMap::from([(project.name.clone(), project)]))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn applying_credential_encrypts_password_and_reapplying_does_not_rewrite_stored_row() {
        let (db, name, project_id) = database_with_project().await;
        let connection = db.connection();
        let dir = credentials_dir_with_passwords(&name, &["s3cret@pass/1"]);

        apply(
            &connection,
            dir.path(),
            state_project(&name, &["Git.Example.com:8443"]),
        )
        .await;
        let first =
            credentials::one_for_project_and_host(&connection, project_id, "git.example.com:8443")
                .await
                .unwrap()
                .expect("the declared credential is stored under its normalized host");
        apply(
            &connection,
            dir.path(),
            state_project(&name, &["git.example.com:8443"]),
        )
        .await;
        let second = credentials::list(&connection, project_id).await.unwrap();

        assert_eq!(first.username, "ci");
        assert_ne!(first.password, "s3cret@pass/1");
        let crypt_file = dir.path().join("crypt");
        assert_eq!(
            gradient_sources::decrypt_secret(crypt_file.to_str().unwrap(), &first.password)
                .unwrap()
                .expose(),
            "s3cret@pass/1"
        );
        assert_eq!(second.len(), 1);
        assert_eq!(
            (&second[0].password, second[0].updated_at),
            (&first.password, first.updated_at)
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn reapplying_after_changing_username_in_state_and_password_in_file_stores_new_username_and_password()
     {
        let (db, name, project_id) = database_with_project().await;
        let connection = db.connection();
        let dir = credentials_dir_with_passwords(&name, &["old-password"]);
        apply(
            &connection,
            dir.path(),
            state_project(&name, &["github.com"]),
        )
        .await;

        std::fs::write(
            dir.path()
                .join(format!("gradient_project_{name}_git_https_0_password")),
            "new-password\n",
        )
        .unwrap();
        let mut rotated = state_project(&name, &["github.com"]);
        rotated.git_https_credentials[0].username = "deploy".into();
        apply(&connection, dir.path(), rotated).await;

        let stored = credentials::list(&connection, project_id).await.unwrap();
        let crypt_file = dir.path().join("crypt");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].username, "deploy");
        assert_eq!(
            gradient_sources::decrypt_secret(crypt_file.to_str().unwrap(), &stored[0].password)
                .unwrap()
                .expose(),
            "new-password"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn git_https_credential_no_longer_declared_in_state_is_deleted_on_next_apply() {
        let (db, name, project_id) = database_with_project().await;
        let connection = db.connection();
        let dir = credentials_dir_with_passwords(&name, &["first", "second"]);

        apply(
            &connection,
            dir.path(),
            state_project(&name, &["github.com", "gitlab.com"]),
        )
        .await;
        apply(
            &connection,
            dir.path(),
            state_project(&name, &["gitlab.com"]),
        )
        .await;

        let hosts: Vec<String> = credentials::list(&connection, project_id)
            .await
            .unwrap()
            .into_iter()
            .map(|credential| credential.host)
            .collect();
        assert_eq!(hosts, ["gitlab.com"]);
        db.finish().await;
    }
}
