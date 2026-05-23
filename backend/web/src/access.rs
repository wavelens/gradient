/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Unified resource-loading and access-control layer.
//!
//! Endpoint handlers declare *what level of access they need* via an enum and
//! receive a fully-validated row, instead of stitching together ad-hoc
//! lookup + permission + state-managed checks. Authorization is expressed in
//! terms of [`Permission`] capabilities (see [`crate::permissions`]) so that
//! custom roles configured at runtime can be plugged in by changing only the
//! permission lookup, not the call sites.
//!
//! Resource families:
//! - Organizations: [`load_org`] with [`OrgAccess`].
//! - Projects: [`load_project`] with [`ProjectAccess`].
//! - Caches: [`load_cache`] with [`CacheAccess`] (owner-scoped, not org-scoped).
//! - Org-scoped children: [`load_webhook_in_org`], [`load_integration_in_org`].

use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::permissions::{Permission, PermissionMask, mask_grants};
use gradient_core::db::{
    get_any_cache_by_name, get_any_organization_by_name, get_any_project_by_name,
};
use gradient_core::types::ids::{IntegrationId, OrganizationId, UserId, WebhookId};
use gradient_core::types::{
    CIntegration, COrganizationUser, CWebhook, EIntegration, EOrganizationUser, ERole, EWebhook,
    MCache, MIntegration, MOrganization, MOrganizationUser, MProject, MUser, MWebhook, ServerState,
};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::sync::Arc;

// ── Caller identity ──────────────────────────────────────────────────────────

/// Who is making the request. Anonymous callers can only access `Readable`
/// resources that are publicly visible.
#[derive(Clone, Copy)]
pub enum Caller<'a> {
    Anon,
    User(&'a MUser),
}

impl<'a> Caller<'a> {
    pub fn from_option(maybe: &'a Option<MUser>) -> Self {
        match maybe {
            Some(u) => Caller::User(u),
            None => Caller::Anon,
        }
    }

    pub fn user_id(&self) -> Option<UserId> {
        match self {
            Caller::User(u) => Some(u.id),
            Caller::Anon => None,
        }
    }
}

// ── Access policies ──────────────────────────────────────────────────────────

/// Required access level for an organization-scoped operation.
#[derive(Clone, Copy)]
pub enum OrgAccess {
    /// Anonymous callers may see public orgs; private orgs require membership
    /// (i.e. `Permission::ViewOrg`). `label` controls the not-found wording -
    /// project endpoints pass `"Project"` so org existence isn't leaked.
    Readable { label: &'static str },

    /// Caller must hold `permission`. Set `reject_managed` to true for
    /// mutating operations that should not apply to state-managed orgs.
    Require {
        permission: Permission,
        reject_managed: bool,
    },

    /// Caller must be a member (any role). Reserved for handlers that
    /// historically don't enforce a specific permission. New code should
    /// prefer [`OrgAccess::Require`].
    Member { reject_managed: bool },
}

/// Required access level for a project-scoped operation.
#[derive(Clone, Copy)]
pub enum ProjectAccess {
    /// Anonymous callers may see projects in public orgs; private orgs
    /// require membership.
    Readable,
    /// Caller must hold `permission` on the owning org.
    Require {
        permission: Permission,
        reject_managed: bool,
    },
    /// Caller must be a member of the owning org (any role).
    Member,
}

#[derive(Clone, Copy)]
pub enum CacheAccess {
    /// Caller must own the cache.
    Owned,
    /// Caller must own the cache and the cache must not be state-managed.
    Editable,
}

// ── Org loader ───────────────────────────────────────────────────────────────

pub async fn load_org(
    state: &Arc<ServerState>,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    org_name: String,
    access: OrgAccess,
) -> WebResult<MOrganization> {
    let label = match access {
        OrgAccess::Readable { label } => label,
        _ => "Organization",
    };

    let org = get_any_organization_by_name(Arc::clone(state), org_name)
        .await?
        .or_not_found(label)?;

    match access {
        OrgAccess::Readable { .. } => {
            if !org.public {
                let visible = match caller.user_id() {
                    Some(uid) => is_org_member(state, uid, org.id, api_key).await?,
                    None => false,
                };
                if !visible {
                    return Err(WebError::not_found(label));
                }
            }
        }
        OrgAccess::Member { reject_managed } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            if !is_org_member(state, uid, org.id, api_key).await? {
                return Err(WebError::not_found(label));
            }
            if reject_managed {
                reject_managed_org(&org)?;
            }
        }
        OrgAccess::Require {
            permission,
            reject_managed,
        } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            require_org_permission(state, uid, org.id, permission, label, api_key).await?;
            if reject_managed {
                reject_managed_org(&org)?;
            }
        }
    }

    Ok(org)
}

// ── Project loader ───────────────────────────────────────────────────────────

pub async fn load_project(
    state: &Arc<ServerState>,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    org_name: String,
    project_name: String,
    access: ProjectAccess,
) -> WebResult<(MOrganization, MProject)> {
    let label = "Project";

    let (org, project) = get_any_project_by_name(Arc::clone(state), org_name, project_name)
        .await?
        .or_not_found(label)?;

    match access {
        ProjectAccess::Readable => {
            if !org.public {
                let visible = match caller.user_id() {
                    Some(uid) => is_org_member(state, uid, org.id, api_key).await?,
                    None => false,
                };
                if !visible {
                    return Err(WebError::not_found(label));
                }
            }
        }
        ProjectAccess::Member => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            if !is_org_member(state, uid, org.id, api_key).await? {
                return Err(WebError::not_found(label));
            }
        }
        ProjectAccess::Require {
            permission,
            reject_managed,
        } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            require_org_permission(state, uid, org.id, permission, label, api_key).await?;
            if reject_managed && project.managed {
                return Err(WebError::forbidden(
                    "Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API.",
                ));
            }
        }
    }

    Ok((org, project))
}

// ── Cache loader ─────────────────────────────────────────────────────────────

pub async fn load_cache(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_name: String,
    access: CacheAccess,
) -> WebResult<MCache> {
    let cache = get_any_cache_by_name(Arc::clone(state), cache_name)
        .await?
        .or_not_found("Cache")?;

    if cache.created_by != user_id {
        return Err(WebError::not_found("Cache"));
    }

    if matches!(access, CacheAccess::Editable) && cache.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API.",
        ));
    }

    Ok(cache)
}

// ── Org-scoped child resources ───────────────────────────────────────────────

pub async fn load_webhook_in_org(
    state: &Arc<ServerState>,
    org_id: OrganizationId,
    webhook_id: WebhookId,
) -> WebResult<MWebhook> {
    EWebhook::find()
        .filter(CWebhook::Id.eq(webhook_id))
        .filter(CWebhook::Organization.eq(org_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Webhook")
}

pub async fn load_integration_in_org(
    state: &Arc<ServerState>,
    org_id: OrganizationId,
    integration_id: IntegrationId,
) -> WebResult<MIntegration> {
    EIntegration::find()
        .filter(CIntegration::Id.eq(integration_id))
        .filter(CIntegration::Organization.eq(org_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Integration")
}

// ── Predicates ───────────────────────────────────────────────────────────────

pub async fn is_org_member(
    state: &Arc<ServerState>,
    user_id: UserId,
    organization_id: OrganizationId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    if let Some(ctx) = api_key
        && let Some(pinned) = ctx.organization
        && pinned != organization_id
    {
        return Ok(false);
    }
    Ok(load_org_membership(state, user_id, organization_id)
        .await?
        .is_some())
}

/// True when the user holds `permission` in `organization_id`.
pub async fn has_permission(
    state: &Arc<ServerState>,
    user_id: UserId,
    organization_id: OrganizationId,
    permission: Permission,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    Ok(
        match load_membership_with_permissions(state, user_id, organization_id, api_key).await? {
            Some((_, mask)) => mask_grants(mask, permission),
            None => false,
        },
    )
}

pub async fn load_org_membership(
    state: &Arc<ServerState>,
    user_id: UserId,
    organization_id: OrganizationId,
) -> WebResult<Option<MOrganizationUser>> {
    Ok(EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization_id))
                .add(COrganizationUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?)
}

/// Load the membership row together with the role's permission bitmask.
///
/// When `api_key` is supplied, callers pinned to a different organization see
/// `None` (the short-circuit looks identical to "not a member"); otherwise the
/// returned mask is the role mask intersected with the key's mask.
///
/// Two queries are issued (membership lookup, then role lookup by id) rather
/// than a JOIN; this keeps the mock-DB test fixtures readable and the second
/// roundtrip is gated on the first returning a row, so the cost is paid only
/// for authenticated members.
pub async fn load_membership_with_permissions(
    state: &Arc<ServerState>,
    user_id: UserId,
    organization_id: OrganizationId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<(MOrganizationUser, PermissionMask)>> {
    if let Some(ctx) = api_key
        && let Some(pinned) = ctx.organization
        && pinned != organization_id
    {
        return Ok(None);
    }
    let Some(membership) = load_org_membership(state, user_id, organization_id).await? else {
        return Ok(None);
    };
    // The `organization_user.role -> role.id` FK is NOT NULL, so a missing
    // role here means the seed step never ran or the row was hand-deleted -
    // treat it as "no permissions" rather than panicking.
    let mask = ERole::find_by_id(membership.role)
        .one(&state.web_db)
        .await?
        .map(|r| r.permission)
        .unwrap_or(0);
    let effective = match api_key {
        Some(ctx) => mask & ctx.mask,
        None => mask,
    };
    Ok(Some((membership, effective)))
}

// ── Internal helpers ─────────────────────────────────────────────────────────

async fn require_org_permission(
    state: &Arc<ServerState>,
    user_id: UserId,
    org_id: OrganizationId,
    permission: Permission,
    not_found_label: &str,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<()> {
    let (_, mask) = load_membership_with_permissions(state, user_id, org_id, api_key)
        .await?
        .ok_or_else(|| WebError::not_found(not_found_label))?;

    if !mask_grants(mask, permission) {
        return Err(WebError::forbidden(
            "You do not have permission to perform this action.",
        ));
    }

    Ok(())
}

fn reject_managed_org(org: &MOrganization) -> WebResult<()> {
    if org.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed organization. This organization is managed by configuration and cannot be edited through the API.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::ApiKeyContext;
    use gradient_core::ci::WebhookClient;
    use gradient_core::permissions::mask_from;
    use gradient_core::storage::{EmailSender, NarStore};
    use gradient_core::types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
    use gradient_core::types::ids::{OrganizationUserId, ProjectId, RoleId};
    use gradient_core::types::{RuntimeConfig, WebDb, WorkerDb};
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

    fn org_fixture(public: bool, managed: bool) -> entity::organization::Model {
        entity::organization::Model {
            id: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            name: "test-org".into(),
            display_name: "Test".into(),
            description: String::new(),
            public_key: "ssh".into(),
            private_key: "enc".into(),
            public,
            hide_build_requests: false,
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
            github_installation_id: None,
        }
    }

    fn project_fixture(managed: bool) -> entity::project::Model {
        entity::project::Model {
            id: ProjectId::new(uuid!("a0000000-0000-0000-0000-000000000002")),
            organization: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            name: "test-project".into(),
            display_name: "Test".into(),
            description: String::new(),
            repository: "git@example.com:test/test.git".into(),
            wildcard: "*".into(),
            active: true,
            last_evaluation: None,
            last_check_at: fixture_date(),
            force_evaluation: false,
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
            keep_evaluations: 30,
            concurrency: 3,
            sign_cache: true,
        }
    }

    fn membership_fixture(role: RoleId) -> entity::organization_user::Model {
        entity::organization_user::Model {
            id: OrganizationUserId::new(uuid!("a0000000-0000-0000-0000-000000000010")),
            organization: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            user: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            role,
        }
    }

    fn role_fixture(id: RoleId, permission: PermissionMask) -> entity::role::Model {
        entity::role::Model {
            id,
            name: "fixture".into(),
            organization: None,
            permission,
            managed: false,
        }
    }

    fn admin_role_row() -> entity::role::Model {
        role_fixture(BASE_ROLE_ADMIN_ID, crate::permissions::admin_mask())
    }

    fn write_role_row() -> entity::role::Model {
        role_fixture(BASE_ROLE_WRITE_ID, crate::permissions::write_mask())
    }

    fn view_role_row() -> entity::role::Model {
        role_fixture(BASE_ROLE_VIEW_ID, crate::permissions::view_mask())
    }

    fn user_fixture() -> MUser {
        entity::user::Model {
            id: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),

            username: "tester".into(),
            name: "Tester".into(),
            email: "t@example.com".into(),
            password: Some("x".into()),
            last_login_at: fixture_date(),
            created_at: fixture_date(),
            email_verified: true,
            email_verification_token: None,
            email_verification_token_expires: None,
            managed: false,
            superuser: false,
            oidc_issuer: None,
            oidc_subject: None,
        }
    }

    fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let cli = test_cli();
        let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
        let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
        Arc::new(ServerState {
            web_db: WebDb::new(db),
            worker_db: WorkerDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            config,
            log_storage: Arc::new(NoopLogStorage),
            webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_core::http::build_client().expect("http client"),
            shutdown: gradient_core::shutdown::Shutdown::new(),
            jwt_secret: gradient_core::types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
        })
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn admin_required() -> OrgAccess {
        OrgAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        }
    }

    #[test]
    fn org_admin_passes() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_org(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                admin_required(),
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn org_admin_view_role_forbidden() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_org(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                admin_required(),
            )
            .await
            .expect_err("view-only must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn org_admin_managed_forbidden() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_org(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                admin_required(),
            )
            .await
            .expect_err("managed must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn org_admin_non_member_not_found() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([Vec::<entity::organization_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_org(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                admin_required(),
            )
            .await
            .expect_err("non-member must be rejected");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn org_writable_write_role_passes() {
        run(async {
            let user = user_fixture();
            let access = OrgAccess::Require {
                permission: Permission::ManageWebhooks,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_WRITE_ID)]])
                .append_query_results([vec![write_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_org(&state, Caller::User(&user), None, "test-org".into(), access).await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn org_writable_view_role_forbidden() {
        run(async {
            let user = user_fixture();
            let access = OrgAccess::Require {
                permission: Permission::ManageWebhooks,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_org(&state, Caller::User(&user), None, "test-org".into(), access)
                .await
                .expect_err("view-only must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn org_member_view_role_passes() {
        run(async {
            let user = user_fixture();
            let access = OrgAccess::Member {
                reject_managed: false,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let r = load_org(&state, Caller::User(&user), None, "test-org".into(), access).await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn org_readable_public_visible_to_anon() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(true, false)]])
                .into_connection();
            let state = make_state(db);
            let r = load_org(
                &state,
                Caller::Anon,
                None,
                "test-org".into(),
                OrgAccess::Readable {
                    label: "Organization",
                },
            )
            .await;
            assert!(r.is_ok());
        });
    }

    #[test]
    fn org_readable_private_invisible_to_anon() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_org(
                &state,
                Caller::Anon,
                None,
                "test-org".into(),
                OrgAccess::Readable {
                    label: "Organization",
                },
            )
            .await
            .expect_err("anon must not see private org");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn project_editable_admin_passes() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Require {
                permission: Permission::EditProject,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![project_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                "test-project".into(),
                access,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn project_editable_view_forbidden() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Require {
                permission: Permission::EditProject,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![project_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                "test-project".into(),
                access,
            )
            .await
            .expect_err("view-only must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn project_editable_managed_forbidden() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Require {
                permission: Permission::EditProject,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![project_fixture(true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                "test-project".into(),
                access,
            )
            .await
            .expect_err("managed must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn project_missing_returns_project_label() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Require {
                permission: Permission::EditProject,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<entity::organization::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                "test-project".into(),
                access,
            )
            .await
            .expect_err("missing must be rejected");
            match err {
                WebError::NotFound(_, msg) => assert!(msg.contains("Project"), "got {}", msg),
                other => panic!("expected NotFound, got {:?}", other),
            }
        });
    }

    fn api_key_ctx(mask: PermissionMask, org: Option<OrganizationId>) -> ApiKeyContext {
        ApiKeyContext {
            api_id: entity::ids::ApiId::new(uuid!("a0000000-0000-0000-0000-000000000099")),
            mask,
            organization: org,
        }
    }

    #[test]
    fn api_key_intersection_caps_admin_user_to_view_only() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(Permission::ViewOrg.bit(), None);
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let access = OrgAccess::Require {
                permission: Permission::ManageMembers,
                reject_managed: true,
            };
            let err = load_org(
                &state,
                Caller::User(&user),
                Some(&key),
                "test-org".into(),
                access,
            )
            .await
            .expect_err("admin user must be capped by view-only key");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn api_key_pinned_to_other_org_returns_not_found() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(
                mask_from(Permission::ALL),
                Some(OrganizationId::new(uuid!(
                    "a0000000-0000-0000-0000-0000000000ff"
                ))),
            );
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .into_connection();
            let state = make_state(db);
            let access = OrgAccess::Member {
                reject_managed: false,
            };
            let err = load_org(
                &state,
                Caller::User(&user),
                Some(&key),
                "test-org".into(),
                access,
            )
            .await
            .expect_err("pinned-elsewhere key must be invisible to this org");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn api_key_pinned_to_matching_org_passes() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(
                mask_from(Permission::ALL),
                Some(OrganizationId::new(uuid!(
                    "a0000000-0000-0000-0000-000000000001"
                ))),
            );
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let access = OrgAccess::Require {
                permission: Permission::ManageMembers,
                reject_managed: false,
            };
            let r = load_org(
                &state,
                Caller::User(&user),
                Some(&key),
                "test-org".into(),
                access,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn session_caller_unaffected_by_api_key_logic() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![org_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_org(
                &state,
                Caller::User(&user),
                None,
                "test-org".into(),
                admin_required(),
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    // ── load_cache ─────────────────────────────────────────────────────────

    fn cache_fixture(managed: bool) -> entity::cache::Model {
        entity::cache::Model {
            id: gradient_core::types::ids::CacheId::new(uuid!(
                "a0000000-0000-0000-0000-000000000020"
            )),
            name: "test-cache".into(),
            display_name: "Test".into(),
            description: String::new(),
            active: true,
            priority: 30,
            local_priority: None,
            public_key: "k".into(),
            private_key: "p".into(),
            public: false,
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
        }
    }

    #[test]
    fn cache_owned_unmanaged_passes() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state,
                user_fixture().id,
                "test-cache".into(),
                CacheAccess::Editable,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn cache_editable_rejects_managed() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(true)]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state,
                user_fixture().id,
                "test-cache".into(),
                CacheAccess::Editable,
            )
            .await
            .expect_err("Editable must reject managed cache");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    /// `Owned` is the access level NAR-content endpoints use: it must permit
    /// the cache owner to mutate content even when the cache's *config* is
    /// state-managed, because NAR storage is operational data, not config.
    #[test]
    fn cache_owned_allows_managed() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(true)]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state,
                user_fixture().id,
                "test-cache".into(),
                CacheAccess::Owned,
            )
            .await;
            assert!(r.is_ok(), "Owned must allow managed cache: {:?}", r.err());
        });
    }

    #[test]
    fn cache_non_owner_returns_not_found() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let other = UserId::new(uuid!("a0000000-0000-0000-0000-000000000099"));
            let err = load_cache(&state, other, "test-cache".into(), CacheAccess::Owned)
                .await
                .expect_err("non-owner must be rejected");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }
}
