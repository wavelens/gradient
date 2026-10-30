/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::StateApplicator;
use super::super::{DynError, lookup_id, read_credential};
use crate::config::StateProject;
use gradient_types::ProjectId;
use gradient_types::exec::validate_secret_name;
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_project_secrets(
        &self,
        projects: &HashMap<String, StateProject>,
    ) -> Result<(), DynError> {
        let project_map = self.project_lookup().await?;

        for project in projects.values() {
            let project_id = lookup_id(&project_map, &project.name, "Project")?;
            self.apply_secrets_of_project(project_id, project).await?;
        }

        Ok(())
    }

    async fn apply_secrets_of_project(
        &self,
        project_id: ProjectId,
        project: &StateProject,
    ) -> Result<(), DynError> {
        let names: Vec<String> = project.secret_files.keys().cloned().collect();
        for name in &names {
            validate_secret_name(name)
                .map_err(|e| format!("Project '{}' secret_files: {e}", project.name))?;
        }

        for name in &names {
            let (value, _) = read_credential(
                self.credentials_dir,
                "project",
                &project.name,
                &format!("secret_{name}"),
                "project secret file",
            )?;
            let encrypted = self.encrypt_to_b64(&value, "project secret")?;
            gradient_db::project_secrets::upsert(self.db, project_id, name, encrypted, true, None)
                .await?;
        }

        let removed =
            gradient_db::project_secrets::delete_managed_except(self.db, project_id, &names)
                .await?;
        if removed > 0 {
            tracing::info!(project = %project.name, removed, "Removed project secrets no longer in state");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, mock_db};
    use gradient_fixtures::rows::project;
    use sea_orm::{DatabaseConnection, MockExecResult};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn state_project(secret_files: &[&str]) -> StateProject {
        StateProject {
            name: "test-project".into(),
            display_name: "Test Project".into(),
            id: None,
            description: None,
            private_key_file: String::new(),
            secret_files: secret_files
                .iter()
                .map(|name| (name.to_string(), format!("/run/secrets/{name}")))
                .collect::<BTreeMap<_, _>>(),
            public: false,
            hide_build_requests: false,
            created_by: "alice".into(),
            members: vec![],
            teams: vec![],
        }
    }

    async fn apply(
        db: &DatabaseConnection,
        credentials: &Path,
        crypt_file: &str,
        project: StateProject,
    ) -> Result<(), DynError> {
        let app = StateApplicator {
            db,
            crypt_secret_file: crypt_file,
            credentials_dir: credentials,
            email_enabled: false,
        };
        app.apply_project_secrets(&HashMap::from([(project.name.clone(), project)]))
            .await
    }

    #[tokio::test]
    async fn an_apply_deletes_managed_secrets_no_longer_listed() {
        let dir =
            std::env::temp_dir().join(format!("gradient-state-secrets-{}", ProjectId::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("gradient_project_test-project_secret_wandb-token"),
            "s3cr3t-value\n",
        )
        .unwrap();
        let crypt_file = dir.join("crypt");
        std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").unwrap();
        let db = mock_db()
            .append_query_results([vec![project()]])
            .append_query_results([vec![gradient_types::MProjectSecret {
                name: "wandb-token".into(),
                managed: true,
                ..Default::default()
            }]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        apply(
            &db,
            &dir,
            crypt_file.to_str().unwrap(),
            state_project(&["wandb-token"]),
        )
        .await
        .unwrap();

        let statements = all_statements(db.into_transaction_log());
        let upsert = statements
            .iter()
            .find(|s| s.sql.starts_with(r#"INSERT INTO "project_secret""#))
            .expect("an upsert");
        assert!(
            format!("{:?}", upsert.values).contains("Bool(Some(true))"),
            "{upsert:?}"
        );
        let delete = statements
            .iter()
            .find(|s| s.sql.starts_with(r#"DELETE FROM "project_secret""#))
            .expect("a delete");
        assert!(delete.sql.contains(r#""managed" = "#), "{}", delete.sql);
        assert!(delete.sql.contains(r#""name" NOT IN ("#), "{}", delete.sql);
        assert!(
            format!("{:?}", delete.values).contains("wandb-token"),
            "{delete:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_bad_secret_name_fails_the_apply() {
        let db = mock_db()
            .append_query_results([vec![project()]])
            .into_connection();

        let err = apply(&db, Path::new(""), "", state_project(&["Bad_Name"]))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Bad_Name"), "{err}");
    }
}
