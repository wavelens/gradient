/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[cfg(test)]
mod tests {
    use crate::*;
    use sea_orm::{
        entity::prelude::*,
        DatabaseBackend, MockDatabase,
    };
    use uuid::Uuid;
    use chrono::NaiveDate;

    #[tokio::test]
    async fn test_user_entity_basic() -> Result<(), DbErr> {
        let user_id = Uuid::new_v4();
        let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![user::Model {
                    id: user_id,
                    username: "testuser".to_owned(),
                    name: "Test User".to_owned(),
                    email: "test@example.com".to_owned(),
                    password: Some("hashed_password".to_owned()),
                    last_login_at: naive_date,
                    created_at: naive_date,
                }],
            ])
            .into_connection();

        let result = user::Entity::find_by_id(user_id).one(&db).await?;
        
        assert!(result.is_some());
        let user = result.unwrap();
        assert_eq!(user.username, "testuser");
        assert_eq!(user.email, "test@example.com");

        Ok(())
    }

    #[tokio::test]
    async fn test_organization_entity_basic() -> Result<(), DbErr> {
        let org_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![organization::Model {
                    id: org_id,
                    name: "test-org".to_owned(),
                    display_name: "Test Organization".to_owned(),
                    description: "Test Description".to_owned(),
                    public_key: "ssh-rsa AAAAB3...".to_owned(),
                    private_key: "-----BEGIN PRIVATE KEY-----".to_owned(),
                    use_nix_store: true,
                    created_by: user_id,
                    created_at: naive_date,
                }],
            ])
            .into_connection();

        let result = organization::Entity::find_by_id(org_id).one(&db).await?;
        
        assert!(result.is_some());
        let org = result.unwrap();
        assert_eq!(org.name, "test-org");
        assert_eq!(org.display_name, "Test Organization");
        assert_eq!(org.created_by, user_id);

        Ok(())
    }

    #[tokio::test]
    async fn test_build_entity_with_status() -> Result<(), DbErr> {
        let build_id = Uuid::new_v4();
        let evaluation_id = Uuid::new_v4();
        let server_id = Uuid::new_v4();
        let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![build::Model {
                    id: build_id,
                    evaluation: evaluation_id,
                    status: build::BuildStatus::Completed,
                    derivation_path: "/nix/store/abc123-hello-world".to_owned(),
                    architecture: server::Architecture::X86_64Linux,
                    server: Some(server_id),
                    log: Some("Build completed successfully".to_owned()),
                    created_at: naive_date,
                    updated_at: naive_date,
                }],
            ])
            .into_connection();

        let result = build::Entity::find_by_id(build_id).one(&db).await?;
        
        assert!(result.is_some());
        let build = result.unwrap();
        assert_eq!(build.derivation_path, "/nix/store/abc123-hello-world");
        assert_eq!(build.status, build::BuildStatus::Completed);
        assert_eq!(build.evaluation, evaluation_id);
        assert_eq!(build.server, Some(server_id));

        Ok(())
    }

    #[tokio::test]
    async fn test_evaluation_entity_with_status() -> Result<(), DbErr> {
        let eval_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let commit_id = Uuid::new_v4();
        let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![evaluation::Model {
                    id: eval_id,
                    project: project_id,
                    repository: "https://github.com/test/repo".to_owned(),
                    commit: commit_id,
                    wildcard: "*".to_owned(),
                    status: evaluation::EvaluationStatus::Completed,
                    previous: None,
                    next: None,
                    created_at: naive_date,
                }],
            ])
            .into_connection();

        let result = evaluation::Entity::find_by_id(eval_id).one(&db).await?;
        
        assert!(result.is_some());
        let eval = result.unwrap();
        assert_eq!(eval.status, evaluation::EvaluationStatus::Completed);
        assert_eq!(eval.project, project_id);
        assert_eq!(eval.commit, commit_id);
        assert_eq!(eval.repository, "https://github.com/test/repo");

        Ok(())
    }

    #[test]
    fn test_build_status_enum() {
        // Test enum values exist and can be compared
        let created = build::BuildStatus::Created;
        let queued = build::BuildStatus::Queued;
        let building = build::BuildStatus::Building;
        let completed = build::BuildStatus::Completed;
        let failed = build::BuildStatus::Failed;
        let aborted = build::BuildStatus::Aborted;
        
        // Test that all enum variants are different
        assert_ne!(created, queued);
        assert_ne!(queued, building);
        assert_ne!(building, completed);
        assert_ne!(completed, failed);
        assert_ne!(failed, aborted);
    }

    #[test]
    fn test_evaluation_status_enum() {
        // Test enum values exist and can be compared
        let queued = evaluation::EvaluationStatus::Queued;
        let evaluating = evaluation::EvaluationStatus::Evaluating;
        let building = evaluation::EvaluationStatus::Building;
        let completed = evaluation::EvaluationStatus::Completed;
        let failed = evaluation::EvaluationStatus::Failed;
        let aborted = evaluation::EvaluationStatus::Aborted;
        
        // Test that all enum variants are different
        assert_ne!(queued, evaluating);
        assert_ne!(evaluating, building);
        assert_ne!(building, completed);
        assert_ne!(completed, failed);
        assert_ne!(failed, aborted);
    }

    #[test]
    fn test_server_architecture_enum() {
        // Test architecture enum values
        let x86_linux = server::Architecture::X86_64Linux;
        let aarch64_linux = server::Architecture::Aarch64Linux;
        let x86_darwin = server::Architecture::X86_64Darwin;
        let aarch64_darwin = server::Architecture::Aarch64Darwin;
        
        // Test that all enum variants are different
        assert_ne!(x86_linux, aarch64_linux);
        assert_ne!(aarch64_linux, x86_darwin);
        assert_ne!(x86_darwin, aarch64_darwin);
    }

    #[test]
    fn test_architecture_from_str() {
        use std::str::FromStr;
        
        assert_eq!(server::Architecture::from_str("x86_64-linux").unwrap(), server::Architecture::X86_64Linux);
        assert_eq!(server::Architecture::from_str("aarch64-linux").unwrap(), server::Architecture::Aarch64Linux);
        assert_eq!(server::Architecture::from_str("x86_64-darwin").unwrap(), server::Architecture::X86_64Darwin);
        assert_eq!(server::Architecture::from_str("aarch64-darwin").unwrap(), server::Architecture::Aarch64Darwin);
        
        assert!(server::Architecture::from_str("invalid-arch").is_err());
    }
}