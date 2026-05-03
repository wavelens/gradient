/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod integrations;
pub mod management;
pub mod members;
pub mod settings;
pub mod ssh;
pub mod workers;

pub use self::integrations::{
    CreateIntegrationRequest, IntegrationResponse, PatchIntegrationRequest, delete_integration,
    get_integration, get_integrations, patch_integration, put_integration,
};
pub use self::management::{
    MakeOrganizationRequest, OrgResponse, OrganizationSummary, PatchOrganizationRequest,
    delete_organization, get, get_org_name_available, get_organization, get_public_organizations,
    patch_organization, put,
};
pub use self::members::{
    AddUserRequest, RemoveUserRequest, StringListItem, delete_organization_users,
    get_organization_users, patch_organization_users, post_organization_users,
};
pub use self::settings::{
    CacheSubscriptionItem, SubscribeCacheRequest, delete_organization_public,
    delete_organization_subscribe_cache, get_organization_subscribe, post_organization_public,
    post_organization_subscribe_cache,
};
pub use self::ssh::{get_organization_ssh, post_organization_ssh};
pub use self::workers::{
    OrgWorkerEntry, PatchWorkerRequest, RegisterWorkerRequest, RegisterWorkerResponse,
    WorkerLiveInfo, delete_org_worker, get_org_workers, patch_org_worker, post_org_worker,
};

use crate::helpers::OptionExt;
use crate::error::{WebError, WebResult};
use gradient_core::db::get_organization_by_name;
use gradient_core::types::consts::BASE_ROLE_ADMIN_ID;
use gradient_core::types::{
    COrganizationUser, EOrganizationUser, MOrganization, MOrganizationUser, ServerState,
};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::sync::Arc;
use uuid::Uuid;

/// Load an organization that the given user is a member of.
///
/// Returns `not_found("Organization")` when the org doesn't exist or the user
/// is not a member, so callers cannot distinguish the two cases.
pub(super) async fn load_org_member(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
) -> WebResult<MOrganization> {
    get_organization_by_name(Arc::clone(state), user_id, org_name)
        .await?
        .or_not_found("Organization")
}

/// Load an organization that the user is a member of AND that is not
/// state-managed.
///
/// Membership ≠ edit rights: this only filters state-managed orgs. Mutating
/// handlers that need admin should additionally call [`load_admin_org`].
pub(super) async fn load_unmanaged_org(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
) -> WebResult<MOrganization> {
    let org = load_org_member(state, user_id, org_name).await?;
    if org.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed organization. This organization is managed by configuration and cannot be edited through the API.".to_string(),
        ));
    }
    Ok(org)
}

/// Load the caller's membership row for `org_id`, or `None` if not a member.
pub(super) async fn load_org_membership(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_id: Uuid,
) -> WebResult<Option<MOrganizationUser>> {
    Ok(EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(org_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?)
}

/// Load an organization that the user administers.
///
/// Requires (1) the org exists, (2) the user is a member, and
/// (3) the user holds the admin role. Also rejects state-managed orgs.
pub(super) async fn load_admin_org(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
) -> WebResult<MOrganization> {
    let org = load_unmanaged_org(state, user_id, org_name).await?;
    let membership = load_org_membership(state, user_id, org.id)
        .await?
        .or_not_found("Organization")?;
    if membership.role != BASE_ROLE_ADMIN_ID {
        return Err(WebError::Forbidden(
            "Admin role required for this operation".to_string(),
        ));
    }
    Ok(org)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WebError;
    use gradient_core::ci::WebhookClient;
    use gradient_core::storage::{EmailSender, NarStore};
    use gradient_core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
    use gradient_core::types::{WebDb, WorkerDb};
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

    fn org_fixture() -> entity::organization::Model {
        entity::organization::Model {
            id: uuid!("a0000000-0000-0000-0000-000000000001"),
            name: "test-org".into(),
            display_name: "Test".into(),
            description: String::new(),
            public_key: "ssh".into(),
            private_key: "enc".into(),
            public: false,
            created_by: uuid!("a0000000-0000-0000-0000-000000000004"),
            created_at: fixture_date(),
            managed: false,
            github_installation_id: None,
        }
    }

    fn membership_fixture(role: Uuid) -> entity::organization_user::Model {
        entity::organization_user::Model {
            id: uuid!("a0000000-0000-0000-0000-000000000010"),
            organization: uuid!("a0000000-0000-0000-0000-000000000001"),
            user: uuid!("a0000000-0000-0000-0000-000000000004"),
            role,
        }
    }

    fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let cli = test_cli();
        let nar_storage = NarStore::local(&cli.storage.base_path).expect("nar store");
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

    #[test]
    fn admin_member_passes() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let user_id = uuid!("a0000000-0000-0000-0000-000000000004");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture()]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .into_connection();
            let state = make_state(db);
            let result = load_admin_org(&state, user_id, "test-org".into()).await;
            assert!(result.is_ok(), "admin should pass: {:?}", result.err());
        });
    }

    #[test]
    fn non_admin_member_is_forbidden() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let user_id = uuid!("a0000000-0000-0000-0000-000000000004");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture()]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let err = load_admin_org(&state, user_id, "test-org".into())
                .await
                .expect_err("view-only role must be rejected");
            assert!(matches!(err, WebError::Forbidden(_)), "got {:?}", err);
        });
    }

    #[test]
    fn non_member_is_not_found() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let user_id = uuid!("a0000000-0000-0000-0000-000000000099");
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<entity::organization::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_admin_org(&state, user_id, "test-org".into())
                .await
                .expect_err("non-member must be rejected");
            assert!(matches!(err, WebError::NotFound(_)), "got {:?}", err);
        });
    }
}
