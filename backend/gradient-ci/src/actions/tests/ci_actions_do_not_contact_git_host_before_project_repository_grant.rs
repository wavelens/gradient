/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::make_ci_context_from_worker_and_web_databases;
use crate::CiContext;
use crate::actions::encrypt_secret_with_file;
use crate::pull_requests::{PatchPullRequest, PullRequestError, open_from_patch};
use gradient_db::git_host_connection_repository_grants::{
    GitHostConnectionRepositoryGrantError, NewGitHostConnectionRepositoryGrant, insert,
};
use gradient_db::rows;
use gradient_entity::{
    commit, evaluation, evaluation_input_update, git_host_connection, project, task, task_action,
    task_action_delivery, team, user,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use gradient_types::*;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::json;
use std::io::Write as _;
use std::sync::Arc;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const GIT_HOST_CONNECTION_MISSING_TASK_REPOSITORY_GRANT_REFUSAL_TEXT: &str =
    "This connection has no grant for this repository in the task's project.";

struct InstanceOwnedGitHostConnectionTestFixture {
    database: TestDatabase,
    db: DatabaseConnection,
    ctx: CiContext,
    git_host: MockServer,
    _crypt_file: tempfile::NamedTempFile,
    project: project::Model,
    connection: git_host_connection::Model,
    task: task::Model,
}

impl InstanceOwnedGitHostConnectionTestFixture {
    async fn create() -> Self {
        let database = TestDatabase::create().await;
        let db = database.connection();
        rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        rows::insert::<team::Entity, _>(&db, gradient_fixtures::rows::team())
            .await
            .expect("team");
        let project = rows::insert::<project::Entity, _>(&db, gradient_fixtures::rows::project())
            .await
            .expect("project");
        let git_host = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "default_branch": "main" })),
            )
            .mount(&git_host)
            .await;
        let mut crypt_file = tempfile::NamedTempFile::new().expect("crypt file");
        crypt_file
            .write_all(b"test-secret-key-32-bytes-padding!")
            .expect("write crypt file");
        let mut ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
        let mut config = (*ctx.db.config).clone();
        config.secrets.crypt_file = crypt_file.path().to_string_lossy().into_owned();
        ctx.db.config = Arc::new(config);
        let access_token = encrypt_secret_with_file(&ctx.db.config.secrets.crypt_file, "token")
            .expect("encrypt access token");
        let connection = rows::insert::<git_host_connection::Entity, _>(
            &db,
            git_host_connection::Model {
                id: GitHostConnectionId::now_v7(),
                name: "instance-gitea".into(),
                display_name: "Gitea".into(),
                git_host_type: GitHostType::Gitea,
                base_url: Some(git_host.uri()),
                reports_status: true,
                access_token: Some(access_token),
                created_by: user_id(),
                created_at: gradient_fixtures::test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("instance connection");
        let task = rows::insert::<task::Entity, _>(
            &db,
            task::Model {
                project: project.id,
                repository: format!("{}/acme/infra", git_host.uri()),
                git_host_connection: Some(connection.id),
                ..gradient_fixtures::rows::task()
            },
        )
        .await
        .expect("task");
        Self {
            database,
            db,
            ctx,
            git_host,
            _crypt_file: crypt_file,
            project,
            connection,
            task,
        }
    }

    async fn insert_repository_grant_for_acme_infra(&self) {
        insert(
            &self.db,
            NewGitHostConnectionRepositoryGrant {
                connection: self.connection.id,
                project: self.project.id,
                repository_owner: "acme",
                repository_name: Some("infra"),
                source: RepositoryGrantSource::Admin,
                created_by: Some(user_id()),
            },
        )
        .await
        .expect("grant");
    }

    async fn test_insert_open_pull_request_task_action_using_instance_connection(
        &self,
    ) -> task_action::Model {
        rows::insert::<task_action::Entity, _>(
            &self.db,
            task_action::Model {
                id: TaskActionId::now_v7(),
                task: self.task.id,
                name: "update inputs".into(),
                action_type: ActionType::OpenPr,
                config: serde_json::to_value(ActionConfig::OpenPr {
                    git_host_connection_id: self.connection.id,
                    generator: Default::default(),
                    granularity: Default::default(),
                    verify_gate: Default::default(),
                    branch_pattern: "gradient/update-{input}".into(),
                    title_template: None,
                    body_template: None,
                    update_existing: true,
                })
                .expect("action config"),
                events: json!(["evaluation.completed"]),
                active: true,
                last_fired_at: None,
                created_by: user_id(),
                created_at: now(),
                updated_at: now(),
            },
        )
        .await
        .expect("open_pr action")
    }

    async fn insert_commit_queued_evaluation_and_bumped_nixpkgs_flake_input_update_returning_evaluation_id(
        &self,
    ) -> EvaluationId {
        let commit = rows::insert::<commit::Entity, _>(
            &self.db,
            gradient_fixtures::rows::build_request_commit(),
        )
        .await
        .expect("commit");
        let evaluation = rows::insert::<evaluation::Entity, _>(
            &self.db,
            gradient_fixtures::rows::queued_evaluation(self.task.id, commit.id),
        )
        .await
        .expect("evaluation");
        rows::insert::<evaluation_input_update::Entity, _>(
            &self.db,
            evaluation_input_update::Model {
                id: EvaluationInputUpdateId::now_v7(),
                evaluation: evaluation.id,
                base_commit: "0".repeat(40),
                generator: "flake".into(),
                target_inputs: json!([]),
                candidate_lock: Some("{}".into()),
                bumped_inputs: Some(json!([{ "name": "nixpkgs", "old_rev": "a", "new_rev": "b" }])),
                discover_only: false,
                created_at: now(),
                updated_at: now(),
            },
        )
        .await
        .expect("input update");
        evaluation.id
    }

    async fn mock_git_host_received_http_request_count(&self) -> usize {
        self.git_host
            .received_requests()
            .await
            .expect("recorded requests")
            .len()
    }

    async fn all_recorded_task_action_delivery_error_messages(&self) -> Vec<Option<String>> {
        task_action_delivery::Entity::find()
            .all(&self.db)
            .await
            .expect("deliveries")
            .into_iter()
            .map(|delivery| delivery.error_message)
            .collect()
    }

    async fn run_open_pull_request_action_for_completed_evaluation_ignoring_result(
        &self,
        action: &task_action::Model,
        evaluation: EvaluationId,
    ) {
        let envelope = json!({ "content": { "evaluation_id": evaluation.to_string() } });
        let _ = crate::actions::execute_action(
            &self.ctx,
            action.clone(),
            "evaluation.completed",
            envelope,
        )
        .await;
    }
}

#[tokio::test]
async fn open_pull_request_with_instance_connection_reaches_git_host_after_repository_grant() {
    let setup = InstanceOwnedGitHostConnectionTestFixture::create().await;
    let action = setup
        .test_insert_open_pull_request_task_action_using_instance_connection()
        .await;
    let evaluation = setup.insert_commit_queued_evaluation_and_bumped_nixpkgs_flake_input_update_returning_evaluation_id().await;

    setup
        .run_open_pull_request_action_for_completed_evaluation_ignoring_result(&action, evaluation)
        .await;

    assert_eq!(setup.mock_git_host_received_http_request_count().await, 0);
    let errors = setup
        .all_recorded_task_action_delivery_error_messages()
        .await;
    assert!(
        matches!(errors.as_slice(), [Some(error)] if error.contains(GIT_HOST_CONNECTION_MISSING_TASK_REPOSITORY_GRANT_REFUSAL_TEXT)),
        "{errors:?}"
    );

    setup.insert_repository_grant_for_acme_infra().await;
    setup
        .run_open_pull_request_action_for_completed_evaluation_ignoring_result(&action, evaluation)
        .await;

    assert!(setup.mock_git_host_received_http_request_count().await > 0);
    setup.database.finish().await;
}

#[tokio::test]
async fn open_pull_request_status_check_with_instance_owned_connection_is_refused_without_repository_grant()
 {
    let setup = InstanceOwnedGitHostConnectionTestFixture::create().await;
    let action = setup
        .test_insert_open_pull_request_task_action_using_instance_connection()
        .await;

    let refused = crate::actions::test_git_host_action(&setup.ctx, &action, &setup.task).await;

    assert!(
        refused.as_ref().is_err_and(|error| error
            .to_string()
            .contains(GIT_HOST_CONNECTION_MISSING_TASK_REPOSITORY_GRANT_REFUSAL_TEXT)),
        "{refused:?}"
    );
    assert_eq!(setup.mock_git_host_received_http_request_count().await, 0);
    let errors = setup
        .all_recorded_task_action_delivery_error_messages()
        .await;
    assert!(
        matches!(errors.as_slice(), [Some(error)] if error.contains(GIT_HOST_CONNECTION_MISSING_TASK_REPOSITORY_GRANT_REFUSAL_TEXT)),
        "{errors:?}"
    );

    setup.insert_repository_grant_for_acme_infra().await;
    crate::actions::test_git_host_action(&setup.ctx, &action, &setup.task)
        .await
        .expect("granted connection reaches the git host");

    assert!(setup.mock_git_host_received_http_request_count().await > 0);
    setup.database.finish().await;
}

#[tokio::test]
async fn pull_request_from_patch_without_repository_grant_is_refused() {
    let setup = InstanceOwnedGitHostConnectionTestFixture::create().await;

    let refused = open_from_patch(
        &setup.ctx,
        PatchPullRequest {
            task: setup.task.clone(),
            git_host_connection: setup.connection.id,
            start_commit: "0".repeat(40),
            patch: b"diff --git a/README b/README\n".to_vec(),
            title: "Update".into(),
            body: String::new(),
            branch: "gradient/update".into(),
            base: None,
            job: None,
        },
    )
    .await;

    assert!(
        matches!(
            refused,
            Err(
                PullRequestError::GitHostConnectionCannotActOnTaskRepository(
                    GitHostConnectionRepositoryGrantError::RepositoryGrantMissingForTaskProject
                )
            )
        ),
        "{refused:?}"
    );
    assert_eq!(setup.mock_git_host_received_http_request_count().await, 0);
    setup.database.finish().await;
}
