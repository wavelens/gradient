/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod evaluations;
pub mod integrations;
pub mod management;
pub mod metrics;

pub use self::evaluations::{
    EntryPointDownloadQuery, EntryPointsQuery, EvaluateRequest, get_entry_point_download,
    get_project_details, get_project_entry_points, get_project_evaluations, post_project_evaluate,
};
pub use self::integrations::{
    delete_project_integration, get_project_integration, put_project_integration,
};
pub use self::management::{
    MakeProjectRequest, PatchProjectRequest, TransferOwnershipRequest, delete_project,
    delete_project_active, get, get_project, get_project_name_available, patch_project,
    post_project_active, post_project_check_repository, post_project_transfer, put,
};
pub use self::metrics::{
    EntryPointMetricsQuery, get_entry_point_metrics, get_project_metrics,
};

use crate::helpers::OptionExt;
use crate::endpoints::get_org_readable;
use crate::error::{WebError, WebResult};
use gradient_core::db::get_project_by_name;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

// ── Shared types ─────────────────────────────────────────────────────────────

use entity::evaluation::EvaluationStatus;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub active: bool,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub last_evaluation: Option<Uuid>,
    pub last_evaluation_status: Option<EvaluationStatus>,
    pub force_evaluation: bool,
    pub created_by: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub can_edit: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointSummary {
    pub id: Uuid,
    pub build_id: Uuid,
    pub derivation_path: String,
    pub eval: String,
    pub build_status: entity::build::BuildStatus,
    pub has_artefacts: bool,
    pub architecture: entity::server::Architecture,
    pub evaluation_id: Uuid,
    pub evaluation_status: EvaluationStatus,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EvaluationSummary {
    pub id: Uuid,
    pub commit: String,
    pub status: EvaluationStatus,
    pub total_builds: i64,
    pub failed_builds: i64,
    pub completed_entry_points: i64,
    pub failed_entry_points: i64,
    pub entry_point_diff: Option<i64>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProjectDetailsResponse {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub repository: String,
    pub evaluation_wildcard: String,
    pub active: bool,
    pub created_at: chrono::NaiveDateTime,
    pub keep_evaluations: i32,
    pub last_evaluations: Vec<EvaluationSummary>,
    pub can_edit: bool,
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Load a project in an org that is readable by `maybe_user`.
///
/// Uses `get_org_readable` so private orgs are invisible to non-members.
/// Returns `not_found("Project")` when the org is inaccessible or the project
/// doesn't exist.
pub(crate) async fn load_readable_project(
    state: &Arc<ServerState>,
    maybe_user: &Option<MUser>,
    org_name: String,
    project_name: String,
) -> WebResult<(MOrganization, MProject)> {
    let organization = get_org_readable(state, org_name, maybe_user, "Project").await?;
    let project = EProject::find()
        .filter(CProject::Organization.eq(organization.id))
        .filter(CProject::Name.eq(project_name))
        .one(&state.web_db)
        .await?
        .or_not_found("Project")?;
    Ok((organization, project))
}

/// Load a project by (org_name, project_name) that the given user is a member of.
///
/// Returns `not_found("Project")` when the org or project doesn't exist, or
/// when the user is not a member of the org.
pub(crate) async fn load_project(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
    project_name: String,
) -> WebResult<(MOrganization, MProject)> {
    get_project_by_name(Arc::clone(state), user_id, org_name, project_name)
        .await?
        .or_not_found("Project")
}

/// Load an editable project: the user must have edit (Admin/Write) permission and
/// the project must not be state-managed.
pub(crate) async fn load_editable_project(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
    project_name: String,
) -> WebResult<(MOrganization, MProject)> {
    let (organization, project) = load_project(state, user_id, org_name, project_name).await?;

    if !user_can_edit(state, user_id, organization.id).await? {
        return Err(WebError::Forbidden(
            "You do not have permission to modify this project.".to_string(),
        ));
    }

    if project.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API.".to_string(),
        ));
    }

    Ok((organization, project))
}

/// Returns true if the user has Admin or Write role in the organization.
pub(crate) async fn user_can_edit(
    state: &Arc<ServerState>,
    user_id: Uuid,
    organization_id: Uuid,
) -> Result<bool, WebError> {
    use gradient_core::types::consts::*;
    let org_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?;

    Ok(match org_user {
        Some(ou) => ou.role == BASE_ROLE_ADMIN_ID || ou.role == BASE_ROLE_WRITE_ID,
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::ci::WebhookClient;
    use gradient_core::storage::{EmailSender, NarStore};
    use gradient_core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use test_support::cli::test_cli;
    use test_support::fakes::email::InMemoryEmailSender;
    use test_support::fakes::webhooks::RecordingWebhookClient;
    use test_support::log_storage::NoopLogStorage;
    use uuid::uuid;

    fn fixture_date() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    fn org_fixture(managed: bool) -> entity::organization::Model {
        entity::organization::Model {
            id: uuid!("b0000000-0000-0000-0000-000000000001"),
            name: "test-org".into(),
            display_name: "Test".into(),
            description: String::new(),
            public_key: "ssh".into(),
            private_key: "enc".into(),
            public: false,
            created_by: uuid!("b0000000-0000-0000-0000-000000000004"),
            created_at: fixture_date(),
            managed,
            github_installation_id: None,
        }
    }

    fn project_fixture(managed: bool) -> entity::project::Model {
        entity::project::Model {
            id: uuid!("b0000000-0000-0000-0000-000000000002"),
            organization: uuid!("b0000000-0000-0000-0000-000000000001"),
            name: "test-project".into(),
            display_name: "Test".into(),
            description: String::new(),
            repository: "git@example.com:test/test.git".into(),
            evaluation_wildcard: "*".into(),
            active: true,
            last_evaluation: None,
            last_check_at: fixture_date(),
            force_evaluation: false,
            created_by: uuid!("b0000000-0000-0000-0000-000000000004"),
            created_at: fixture_date(),
            managed,
            keep_evaluations: 30,
        }
    }

    fn membership_fixture(role: Uuid) -> entity::organization_user::Model {
        entity::organization_user::Model {
            id: uuid!("b0000000-0000-0000-0000-000000000010"),
            organization: uuid!("b0000000-0000-0000-0000-000000000001"),
            user: uuid!("b0000000-0000-0000-0000-000000000004"),
            role,
        }
    }

    fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let cli = test_cli();
        let nar_storage = NarStore::local(&cli.base_path).expect("nar store");
        Arc::new(ServerState {
            web_db: WebDb::new(db),
            worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            cli,
            log_storage: Arc::new(NoopLogStorage),
            webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        })
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn editable_project_admin_passes() {
        run(async {
            let user_id = uuid!("b0000000-0000-0000-0000-000000000004");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false)]])
                .append_query_results([vec![project_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .into_connection();
            let state = make_state(db);
            let res =
                load_editable_project(&state, user_id, "test-org".into(), "test-project".into())
                    .await;
            assert!(res.is_ok(), "admin should pass: {:?}", res.err());
        });
    }

    #[test]
    fn editable_project_write_passes() {
        run(async {
            let user_id = uuid!("b0000000-0000-0000-0000-000000000004");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false)]])
                .append_query_results([vec![project_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_WRITE_ID)]])
                .into_connection();
            let state = make_state(db);
            let res =
                load_editable_project(&state, user_id, "test-org".into(), "test-project".into())
                    .await;
            assert!(res.is_ok(), "write role should pass: {:?}", res.err());
        });
    }

    #[test]
    fn editable_project_view_is_forbidden() {
        run(async {
            let user_id = uuid!("b0000000-0000-0000-0000-000000000004");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false)]])
                .append_query_results([vec![project_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let err =
                load_editable_project(&state, user_id, "test-org".into(), "test-project".into())
                    .await
                    .expect_err("view-only role must be rejected");
            assert!(matches!(err, WebError::Forbidden(_)), "got {:?}", err);
        });
    }

    #[test]
    fn editable_project_non_member_is_not_found() {
        run(async {
            let user_id = uuid!("b0000000-0000-0000-0000-000000000099");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<entity::organization::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err =
                load_editable_project(&state, user_id, "test-org".into(), "test-project".into())
                    .await
                    .expect_err("non-member must be rejected");
            assert!(matches!(err, WebError::NotFound(_)), "got {:?}", err);
        });
    }

    #[test]
    fn editable_project_managed_is_forbidden() {
        run(async {
            let user_id = uuid!("b0000000-0000-0000-0000-000000000004");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false)]])
                .append_query_results([vec![project_fixture(true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .into_connection();
            let state = make_state(db);
            let err =
                load_editable_project(&state, user_id, "test-org".into(), "test-project".into())
                    .await
                    .expect_err("state-managed project must be rejected");
            assert!(matches!(err, WebError::Forbidden(_)), "got {:?}", err);
        });
    }
}
