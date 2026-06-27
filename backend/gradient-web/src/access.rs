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
use crate::permissions::{
    CachePermission, Permission, PermissionMask, cache_mask_grants, mask_grants,
};
use gradient_db::{
    get_any_cache_by_name, get_any_organization_by_name, get_any_project_by_name,
};
use gradient_types::ids::{CacheId, IntegrationId, OrganizationId, UserId};
use gradient_types::{
    CCacheUser, CIntegration, COrganizationCache, COrganizationUser, ECacheRole, ECacheUser,
    EIntegration, EOrganizationCache, EOrganizationUser, ERole, MCache, MIntegration, MOrganization,
    MOrganizationUser, MProject, MUser,
};
use gradient_core::ServerState;
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
    /// Anonymous callers may see public caches; private caches require an
    /// authenticated member (with `ViewCache`). NAR routes still grant
    /// anonymous read via `cache.public` separately, before `load_cache`
    /// is hit.
    Readable,
    /// Caller must hold `permission` on the cache.
    Require {
        permission: CachePermission,
        reject_managed: bool,
    },
    /// Caller must be a member (any role). Used for member/role listing.
    Member { reject_managed: bool },
}

// ── Org loader ───────────────────────────────────────────────────────────────

pub async fn load_org(
    state: &Arc<ServerState>,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    org_name: String,
    access: OrgAccess,
) -> WebResult<MOrganization> {
    if api_key.is_some_and(|k| k.cache_pin.is_some()) {
        return Err(WebError::forbidden(
            "Cache-pinned API key cannot be used on this endpoint.",
        ));
    }

    let label = match access {
        OrgAccess::Readable { label } => label,
        _ => "Organization",
    };

    let org = get_any_organization_by_name(&state.db(), org_name)
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
    if api_key.is_some_and(|k| k.cache_pin.is_some()) {
        return Err(WebError::forbidden(
            "Cache-pinned API key cannot be used on this endpoint.",
        ));
    }

    let label = "Project";

    let (org, project) = get_any_project_by_name(&state.db(), org_name, project_name)
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
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    cache_name: String,
    access: CacheAccess,
) -> WebResult<MCache> {
    let label = "Cache";

    let cache = get_any_cache_by_name(&state.db(), cache_name)
        .await?
        .or_not_found(label)?;

    if let Some(key) = api_key
        && let Some(pin) = key.cache_pin
        && pin != cache.id
    {
        return Err(WebError::forbidden(
            "API key is pinned to a different cache.",
        ));
    }

    match access {
        CacheAccess::Readable => {
            if !cache.public {
                let visible = match caller.user_id() {
                    Some(uid) => {
                        is_cache_member(state, uid, cache.id, api_key).await?
                            || is_cache_org_subscriber(state, uid, cache.id, api_key).await?
                    }
                    None => false,
                };
                if !visible {
                    return Err(WebError::not_found(label));
                }
            }
        }
        CacheAccess::Member { reject_managed } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            if !is_cache_member(state, uid, cache.id, api_key).await? {
                return Err(WebError::not_found(label));
            }
            if reject_managed {
                reject_managed_cache(&cache)?;
            }
        }
        CacheAccess::Require {
            permission,
            reject_managed,
        } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            require_cache_permission(state, uid, cache.id, permission, label, api_key).await?;
            if reject_managed {
                reject_managed_cache(&cache)?;
            }
        }
    }

    Ok(cache)
}

// ── Org-scoped child resources ───────────────────────────────────────────────

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

async fn is_cache_member(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    _api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    let row = ECacheUser::find()
        .filter(CCacheUser::Cache.eq(cache_id))
        .filter(CCacheUser::User.eq(user_id))
        .one(&state.web_db)
        .await?;
    Ok(row.is_some())
}

/// True when `user_id` belongs to an organization that subscribes to `cache_id`.
/// Mirrors `GET /caches` visibility so a cache the user can list is also
/// readable, even without a direct `cache_user` membership row.
async fn is_cache_org_subscriber(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    let subscriber_orgs: Vec<OrganizationId> = EOrganizationCache::find()
        .filter(COrganizationCache::Cache.eq(cache_id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|oc| oc.organization)
        .collect();

    let allowed: Vec<OrganizationId> = match api_key.and_then(|k| k.organization) {
        Some(pinned) => subscriber_orgs.into_iter().filter(|o| *o == pinned).collect(),
        None => subscriber_orgs,
    };
    if allowed.is_empty() {
        return Ok(false);
    }

    let member = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user_id))
        .filter(COrganizationUser::Organization.is_in(allowed))
        .one(&state.web_db)
        .await?;
    Ok(member.is_some())
}

/// The caller's effective cache permission mask for minting cache-scoped API
/// keys: a direct cache member's role mask, or read-only [`cache_view_mask`]
/// for a member of a subscribed organization (mirroring `load_cache(Readable)`).
/// `None` means the caller cannot see the cache.
pub async fn effective_cache_mask(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<i64>> {
    if let Some(mask) = cache_role_mask(state, user_id, cache_id).await? {
        return Ok(Some(mask));
    }
    if is_cache_org_subscriber(state, user_id, cache_id, api_key).await? {
        return Ok(Some(crate::permissions::cache_view_mask()));
    }
    Ok(None)
}

async fn cache_role_mask(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
) -> WebResult<Option<i64>> {
    let mem = ECacheUser::find()
        .filter(CCacheUser::Cache.eq(cache_id))
        .filter(CCacheUser::User.eq(user_id))
        .one(&state.web_db)
        .await?;
    let Some(mem) = mem else { return Ok(None) };
    let role = ECacheRole::find_by_id(mem.role)
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("Cache Role"))?;
    Ok(Some(role.permission))
}

async fn require_cache_permission(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    permission: CachePermission,
    label: &'static str,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<()> {
    let role_mask = cache_role_mask(state, user_id, cache_id)
        .await?
        .ok_or_else(|| WebError::not_found(label))?;
    let key_mask = api_key
        .and_then(|k| k.cache_permission_mask)
        .unwrap_or(i64::MAX);
    let effective = role_mask & key_mask;
    if !cache_mask_grants(effective, permission) {
        return Err(WebError::forbidden(format!(
            "Missing cache permission `{}`.",
            permission.as_wire_name()
        )));
    }
    Ok(())
}

fn reject_managed_cache(cache: &MCache) -> WebResult<()> {
    if cache.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::ApiKeyContext;
    use gradient_db::permissions::mask_from;
    use gradient_storage::{EmailSender, NarStore};
    use gradient_types::consts::{
        BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
    };
    use gradient_types::ids::{OrganizationUserId, ProjectId, RoleId};
    use gradient_types::{RuntimeConfig};
    use gradient_db::{WebDb, WorkerDb};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use gradient_test_support::cli::test_cli;
    use gradient_test_support::fakes::email::InMemoryEmailSender;
    use gradient_test_support::log_storage::NoopLogStorage;
    use uuid::uuid;

    fn fixture_date() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    fn org_fixture(public: bool, managed: bool) -> gradient_entity::organization::Model {
        gradient_entity::organization::Model {
            id: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            name: "test-org".into(),
            display_name: "Test".into(),
            public_key: "ssh".into(),
            private_key: "enc".into(),
            public,
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
            ..Default::default()
        }
    }

    fn project_fixture(managed: bool) -> gradient_entity::project::Model {
        gradient_entity::project::Model {
            id: ProjectId::new(uuid!("a0000000-0000-0000-0000-000000000002")),
            organization: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            name: "test-project".into(),
            display_name: "Test".into(),
            repository: "git@example.com:test/test.git".into(),
            wildcard: "*".into(),
            active: true,
            last_check_at: fixture_date(),
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
            keep_evaluations: 30,
            concurrency: 3,
            sign_cache: true,
            ..Default::default()
        }
    }

    fn membership_fixture(role: RoleId) -> gradient_entity::organization_user::Model {
        gradient_entity::organization_user::Model {
            id: OrganizationUserId::new(uuid!("a0000000-0000-0000-0000-000000000010")),
            organization: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            user: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            role,
        }
    }

    fn role_fixture(id: RoleId, permission: PermissionMask) -> gradient_entity::role::Model {
        gradient_entity::role::Model {
            id,
            name: "fixture".into(),
            permission,
            ..Default::default()
        }
    }

    fn admin_role_row() -> gradient_entity::role::Model {
        role_fixture(BASE_ROLE_ADMIN_ID, crate::permissions::admin_mask())
    }

    fn write_role_row() -> gradient_entity::role::Model {
        role_fixture(BASE_ROLE_WRITE_ID, crate::permissions::write_mask())
    }

    fn view_role_row() -> gradient_entity::role::Model {
        role_fixture(BASE_ROLE_VIEW_ID, crate::permissions::view_mask())
    }

    fn user_fixture() -> MUser {
        gradient_entity::user::Model {
            id: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            username: "tester".into(),
            name: "Tester".into(),
            email: "t@example.com".into(),
            password: Some("x".into()),
            last_login_at: fixture_date(),
            created_at: fixture_date(),
            email_verified: true,
            ..Default::default()
        }
    }

    fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let cli = test_cli();
        let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
        let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
        Arc::new(ServerState {
            web_db: WebDb::new(db),
        cache_db: gradient_db::CacheDb::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection()),
            worker_db: WorkerDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            config,
            log_storage: Arc::new(NoopLogStorage),
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_util::http::build_client().expect("http client"),
            shutdown: gradient_util::shutdown::Shutdown::new(),
            jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
            scim_group_roles: std::sync::Arc::new(Default::default()),
            board_events: tokio::sync::broadcast::channel(256).0,
            forge: gradient_forge::ForgeRegistry::with_builtin(),
            upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            reactor: std::sync::Arc::new(gradient_db::NoReactor),
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
                .append_query_results([Vec::<gradient_entity::organization_user::Model>::new()])
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
                permission: Permission::ManageActions,
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
                permission: Permission::ManageActions,
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
                .append_query_results([Vec::<gradient_entity::organization::Model>::new()])
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
            api_id: gradient_entity::ids::ApiId::new(uuid!("a0000000-0000-0000-0000-000000000099")),
            mask,
            organization: org,
            cache_pin: None,
            cache_permission_mask: None,
            allowed_ips: Vec::new(),
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

    fn cache_fixture(managed: bool) -> gradient_entity::cache::Model {
        gradient_entity::cache::Model {
            id: gradient_types::ids::CacheId::new(uuid!(
                "a0000000-0000-0000-0000-000000000020"
            )),
            name: "test-cache".into(),
            display_name: "Test".into(),
            active: true,
            priority: 30,
            public_key: "k".into(),
            private_key: "p".into(),
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
            ..Default::default()
        }
    }

    fn manage_settings_access() -> CacheAccess {
        CacheAccess::Require {
            permission: CachePermission::ManageCacheSettings,
            reject_managed: true,
        }
    }

    fn write_store_access() -> CacheAccess {
        CacheAccess::Require {
            permission: CachePermission::WriteStore,
            reject_managed: false,
        }
    }

    fn cache_member_fixture() -> gradient_entity::cache_user::Model {
        gradient_entity::cache_user::Model {
            id: gradient_types::ids::CacheUserId::new(uuid!(
                "a0000000-0000-0000-0000-000000000030"
            )),
            cache: gradient_types::ids::CacheId::new(uuid!(
                "a0000000-0000-0000-0000-000000000020"
            )),
            user: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            role: gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID,
        }
    }

    fn cache_role_fixture() -> gradient_entity::cache_role::Model {
        gradient_entity::cache_role::Model {
            id: gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID,
            name: "Admin".into(),
            permission: crate::permissions::cache_admin_mask(),
            managed: true,
            ..Default::default()
        }
    }

    #[test]
    fn cache_manage_settings_passes_for_member() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state,
                Caller::User(&user),
                None,
                "test-cache".into(),
                manage_settings_access(),
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn cache_manage_settings_rejects_managed() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(true)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state,
                Caller::User(&user),
                None,
                "test-cache".into(),
                manage_settings_access(),
            )
            .await
            .expect_err("ManageCacheSettings must reject managed cache");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn cache_write_store_allows_managed() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(true)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state,
                Caller::User(&user),
                None,
                "test-cache".into(),
                write_store_access(),
            )
            .await;
            assert!(
                r.is_ok(),
                "WriteStore must allow managed cache: {:?}",
                r.err()
            );
        });
    }

    #[test]
    fn cache_non_member_returns_not_found() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state,
                Caller::User(&user),
                None,
                "test-cache".into(),
                write_store_access(),
            )
            .await
            .expect_err("non-member must be rejected");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    fn cache_fixture_public(managed: bool) -> gradient_entity::cache::Model {
        let mut c = cache_fixture(managed);
        c.public = true;
        c
    }

    fn cache_role_view_fixture() -> gradient_entity::cache_role::Model {
        gradient_entity::cache_role::Model {
            id: BASE_CACHE_ROLE_VIEW_ID,
            name: "View".into(),
            permission: crate::permissions::cache_view_mask(),
            managed: true,
            ..Default::default()
        }
    }

    fn cache_view_api_key(user_id: UserId) -> ApiKeyContext {
        let _ = user_id;
        ApiKeyContext {
            api_id: gradient_entity::ids::ApiId::new(uuid!("a0000000-0000-0000-0000-000000000099")),
            mask: i64::MAX,
            organization: None,
            cache_pin: None,
            cache_permission_mask: Some(crate::permissions::cache_view_mask()),
            allowed_ips: Vec::new(),
        }
    }

    #[test]
    fn cache_readable_allows_public_anon() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture_public(false)]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state,
                Caller::Anon,
                None,
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await;
            assert!(
                r.is_ok(),
                "anon read on public cache must succeed: {:?}",
                r.err()
            );
        });
    }

    #[test]
    fn cache_readable_blocks_anon_on_private() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state,
                Caller::Anon,
                None,
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await
            .expect_err("anon on private cache must be rejected");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn cache_member_allows_member() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state,
                Caller::User(&user),
                None,
                "test-cache".into(),
                CacheAccess::Member {
                    reject_managed: false,
                },
            )
            .await;
            assert!(r.is_ok(), "member access must succeed: {:?}", r.err());
        });
    }

    #[test]
    fn cache_require_blocks_when_role_lacks_permission() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_view_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state,
                Caller::User(&user),
                None,
                "test-cache".into(),
                CacheAccess::Require {
                    permission: CachePermission::WriteStore,
                    reject_managed: false,
                },
            )
            .await
            .expect_err("View role must lack WriteStore");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    fn org_cache_fixture() -> gradient_entity::organization_cache::Model {
        gradient_entity::organization_cache::Model {
            id: gradient_types::ids::OrganizationCacheId::new(uuid!(
                "a0000000-0000-0000-0000-000000000040"
            )),
            organization: OrganizationId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            cache: gradient_types::ids::CacheId::new(uuid!(
                "a0000000-0000-0000-0000-000000000020"
            )),
            mode: gradient_entity::organization_cache::CacheSubscriptionMode::ReadOnly,
        }
    }

    #[test]
    fn effective_cache_mask_returns_role_for_member() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let mask = effective_cache_mask(&state, user.id, cache_fixture(false).id, None)
                .await
                .expect("query ok")
                .expect("member has a mask");
            assert_eq!(mask, crate::permissions::cache_admin_mask());
        });
    }

    #[test]
    fn effective_cache_mask_returns_view_for_org_subscriber() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .append_query_results([vec![org_cache_fixture()]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let mask = effective_cache_mask(&state, user.id, cache_fixture(false).id, None)
                .await
                .expect("query ok")
                .expect("subscriber gets view-only mask");
            assert_eq!(mask, crate::permissions::cache_view_mask());
        });
    }

    #[test]
    fn effective_cache_mask_none_for_outsider() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .append_query_results([Vec::<gradient_entity::organization_cache::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let mask = effective_cache_mask(&state, user.id, cache_fixture(false).id, None)
                .await
                .expect("query ok");
            assert!(mask.is_none(), "outsider must not get a cache mask");
        });
    }

    #[test]
    fn cache_require_intersects_with_api_key_mask() {
        run(async {
            let user = user_fixture();
            let api_key = cache_view_api_key(user.id);
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state,
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                CacheAccess::Require {
                    permission: CachePermission::WriteStore,
                    reject_managed: false,
                },
            )
            .await
            .expect_err("API key View mask must block WriteStore even with Admin role");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }
}
