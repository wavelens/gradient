/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::permissions::{
    CachePermission, Permission, PermissionMask, cache_mask_grants, mask_grants,
};
use gradient_core::ServerState;
use gradient_db::lookup::{get_any_cache_by_name, get_any_project_by_name, get_any_task_by_name};
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_ROLE_ADMIN_ID};
use gradient_types::ids::{CacheId, IntegrationId, ProjectId, UserId};
use gradient_types::{
    CCache, CCacheAccess, CCacheUser, CIntegration, CProjectAccess, CProjectCache, CProjectUser,
    CUser, ECacheAccess, ECacheUser, EIntegration, EProjectAccess, EProjectCache, EProjectUser,
    EUser, MCache, MIntegration, MProject, MProjectUser, MTask, MUser,
};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::sync::Arc;

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

#[derive(Clone, Copy)]
pub enum ProjectAccess {
    /// Private projects require membership. Task endpoints are passing `"Task"` as `label` so a
    /// not-found answer cannot leak that the project exists.
    Readable {
        label: &'static str,
    },

    Require {
        permission: Permission,
        reject_managed: bool,
    },

    Member {
        reject_managed: bool,
    },
}

#[derive(Clone, Copy)]
pub enum TaskAccess {
    Readable,
    Require {
        permission: Permission,
        reject_managed: bool,
    },
    Member,
}

#[derive(Clone, Copy)]
pub enum CacheAccess {
    Readable,
    Require {
        permission: CachePermission,
        reject_managed: bool,
    },
    Member {
        reject_managed: bool,
    },
}

pub async fn load_project(
    state: &Arc<ServerState>,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    project_name: String,
    access: ProjectAccess,
) -> WebResult<MProject> {
    if api_key.is_some_and(|k| k.cache_pin.is_some()) {
        return Err(WebError::forbidden(
            "Cache-pinned API key cannot be used on this endpoint.",
        ));
    }

    let label = match access {
        ProjectAccess::Readable { label } => label,
        _ => "Project",
    };

    let project = get_any_project_by_name(&state.db(), project_name)
        .await?
        .or_not_found(label)?;

    match access {
        ProjectAccess::Readable { .. } => {
            if !project.public {
                let visible = match caller.user_id() {
                    Some(uid) => is_project_member(state, uid, project.id, api_key).await?,
                    None => false,
                };
                if !visible {
                    return Err(WebError::not_found(label));
                }
            }
        }
        ProjectAccess::Member { reject_managed } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            if !is_project_member(state, uid, project.id, api_key).await? {
                return Err(WebError::not_found(label));
            }
            if reject_managed {
                reject_managed_project(&project)?;
            }
        }
        ProjectAccess::Require {
            permission,
            reject_managed,
        } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            require_project_permission(state, uid, project.id, permission, label, api_key).await?;
            if reject_managed {
                reject_managed_project(&project)?;
            }
        }
    }

    Ok(project)
}

pub async fn load_task(
    state: &Arc<ServerState>,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    project_name: String,
    task_name: String,
    access: TaskAccess,
) -> WebResult<(MProject, MTask)> {
    if api_key.is_some_and(|k| k.cache_pin.is_some()) {
        return Err(WebError::forbidden(
            "Cache-pinned API key cannot be used on this endpoint.",
        ));
    }

    let label = "Task";

    let (project, task) = get_any_task_by_name(&state.db(), project_name, task_name)
        .await?
        .or_not_found(label)?;

    match access {
        TaskAccess::Readable => {
            if !project.public {
                let visible = match caller.user_id() {
                    Some(uid) => is_project_member(state, uid, project.id, api_key).await?,
                    None => false,
                };
                if !visible {
                    return Err(WebError::not_found(label));
                }
            }
        }
        TaskAccess::Member => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            if !is_project_member(state, uid, project.id, api_key).await? {
                return Err(WebError::not_found(label));
            }
        }
        TaskAccess::Require {
            permission,
            reject_managed,
        } => {
            let uid = caller.user_id().ok_or_else(|| WebError::not_found(label))?;
            require_project_permission(state, uid, project.id, permission, label, api_key).await?;
            if reject_managed && task.managed {
                return Err(WebError::forbidden(
                    "Cannot modify state-managed task. This task is managed by configuration and cannot be edited through the API.",
                ));
            }
        }
    }

    Ok((project, task))
}

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
                            || is_cache_project_subscriber(state, uid, cache.id, api_key).await?
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

pub async fn visible_cache_condition(
    state: &Arc<ServerState>,
    user_id: UserId,
) -> WebResult<Condition> {
    let project_ids: Vec<ProjectId> = EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user_id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|m| m.project)
        .collect();

    let project_cache_ids: Vec<CacheId> = if project_ids.is_empty() {
        Vec::new()
    } else {
        EProjectCache::find()
            .filter(CProjectCache::Project.is_in(project_ids))
            .all(&state.web_db)
            .await?
            .into_iter()
            .map(|oc| oc.cache)
            .collect()
    };

    Ok(Condition::any()
        .add(CCache::CreatedBy.eq(user_id))
        .add(CCache::Id.is_in(project_cache_ids)))
}

pub async fn load_integration_in_project(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    integration_id: IntegrationId,
) -> WebResult<MIntegration> {
    EIntegration::find()
        .filter(CIntegration::Id.eq(integration_id))
        .filter(CIntegration::Project.eq(project_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Integration")
}

pub async fn is_project_member(
    state: &Arc<ServerState>,
    user_id: UserId,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    if let Some(ctx) = api_key
        && let Some(pinned) = ctx.project
        && pinned != project_id
    {
        return Ok(false);
    }
    Ok(gradient_db::access::reaches_project(&state.web_db, project_id, user_id).await?)
}

pub async fn load_project_membership(
    state: &Arc<ServerState>,
    user_id: UserId,
    project_id: ProjectId,
) -> WebResult<Option<MProjectUser>> {
    Ok(EProjectUser::find()
        .filter(
            Condition::all()
                .add(CProjectUser::Project.eq(project_id))
                .add(CProjectUser::User.eq(user_id)),
        )
        .one(&state.web_db)
        .await?)
}

pub async fn has_permission(
    state: &Arc<ServerState>,
    user_id: UserId,
    project_id: ProjectId,
    permission: Permission,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    Ok(
        load_membership_with_permissions(state, user_id, project_id, api_key)
            .await?
            .is_some_and(|mask| mask_grants(mask, permission)),
    )
}

pub async fn load_membership_with_permissions(
    state: &Arc<ServerState>,
    user_id: UserId,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<PermissionMask>> {
    if let Some(ctx) = api_key
        && let Some(pinned) = ctx.project
        && pinned != project_id
    {
        return Ok(None);
    }
    let Some(mask) =
        gradient_db::access::project_permission_mask(&state.web_db, project_id, user_id).await?
    else {
        return Ok(None);
    };
    Ok(Some(match api_key {
        Some(ctx) => mask & ctx.mask,
        None => mask,
    }))
}

async fn require_project_permission(
    state: &Arc<ServerState>,
    user_id: UserId,
    project_id: ProjectId,
    permission: Permission,
    not_found_label: &str,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<()> {
    let mask = load_membership_with_permissions(state, user_id, project_id, api_key)
        .await?
        .ok_or_else(|| WebError::not_found(not_found_label))?;

    if !mask_grants(mask, permission) {
        return Err(WebError::forbidden(
            "You do not have permission to perform this action.",
        ));
    }

    Ok(())
}

fn reject_managed_project(project: &MProject) -> WebResult<()> {
    if project.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed project. This project is managed by configuration and cannot be edited through the API.",
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
    Ok(ECacheAccess::find()
        .filter(CCacheAccess::Cache.eq(cache_id))
        .filter(CCacheAccess::User.eq(user_id))
        .one(&state.web_db)
        .await?
        .is_some())
}

async fn is_cache_project_subscriber(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    let subscriber_projects: Vec<ProjectId> = EProjectCache::find()
        .filter(CProjectCache::Cache.eq(cache_id))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|oc| oc.project)
        .collect();

    let allowed: Vec<ProjectId> = match api_key.and_then(|k| k.project) {
        Some(pinned) => subscriber_projects
            .into_iter()
            .filter(|o| *o == pinned)
            .collect(),
        None => subscriber_projects,
    };
    if allowed.is_empty() {
        return Ok(false);
    }

    let member = EProjectAccess::find()
        .filter(CProjectAccess::User.eq(user_id))
        .filter(CProjectAccess::Project.is_in(allowed))
        .one(&state.web_db)
        .await?;
    Ok(member.is_some())
}

pub async fn effective_cache_mask(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<i64>> {
    if let Some(mask) = cache_role_mask(state, user_id, cache_id).await? {
        return Ok(Some(mask));
    }
    if is_cache_project_subscriber(state, user_id, cache_id, api_key).await? {
        return Ok(Some(crate::permissions::cache_view_mask()));
    }
    Ok(None)
}

async fn cache_role_mask(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
) -> WebResult<Option<i64>> {
    Ok(gradient_db::access::cache_permission_mask(&state.web_db, cache_id, user_id).await?)
}

fn cache_mask_allows(
    role_mask: i64,
    api_key: Option<&ApiKeyContext>,
    permission: CachePermission,
) -> bool {
    let key_mask = api_key
        .and_then(|k| k.cache_permission_mask)
        .unwrap_or(i64::MAX);

    cache_mask_grants(role_mask & key_mask, permission)
}

pub async fn has_cache_permission(
    state: &Arc<ServerState>,
    user_id: UserId,
    cache_id: CacheId,
    permission: CachePermission,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    let Some(role_mask) = cache_role_mask(state, user_id, cache_id).await? else {
        return Ok(false);
    };

    Ok(cache_mask_allows(role_mask, api_key, permission))
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

    if !cache_mask_allows(role_mask, api_key, permission) {
        return Err(WebError::forbidden(format!(
            "Missing cache permission `{}`.",
            permission.as_wire_name()
        )));
    }

    Ok(())
}

pub async fn project_admin_emails(
    state: &Arc<ServerState>,
    project_id: ProjectId,
) -> WebResult<Vec<String>> {
    let admin_ids: Vec<UserId> = EProjectUser::find()
        .filter(CProjectUser::Project.eq(project_id))
        .filter(CProjectUser::Role.eq(BASE_ROLE_ADMIN_ID))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|m| m.user)
        .collect();

    admin_emails(state, admin_ids).await
}

pub async fn cache_admin_emails(
    state: &Arc<ServerState>,
    cache_id: CacheId,
) -> WebResult<Vec<String>> {
    let admin_ids: Vec<UserId> = ECacheUser::find()
        .filter(CCacheUser::Cache.eq(cache_id))
        .filter(CCacheUser::Role.eq(BASE_CACHE_ROLE_ADMIN_ID))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|m| m.user)
        .collect();

    admin_emails(state, admin_ids).await
}

async fn admin_emails(state: &Arc<ServerState>, ids: Vec<UserId>) -> WebResult<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    Ok(EUser::find()
        .filter(CUser::Id.is_in(ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .filter(|u| u.active && !u.email.is_empty())
        .map(|u| u.email)
        .collect())
}

pub(crate) fn reject_managed_cache(cache: &MCache) -> WebResult<()> {
    if cache.managed {
        return Err(WebError::forbidden(
            "Cannot modify state-managed cache. This cache is managed by configuration and cannot be edited through the API.",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub enum TeamAccess {
    Member,
    Admin { reject_managed: bool },
}

pub async fn load_team(
    state: &Arc<ServerState>,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
    team_name: String,
    access: TeamAccess,
) -> WebResult<(MTeam, Option<TeamRole>)> {
    if api_key.is_some_and(|k| k.project.is_some() || k.cache_pin.is_some()) {
        return Err(WebError::forbidden(
            "A pinned API key cannot be used on team endpoints.",
        ));
    }

    let team = ETeam::find()
        .filter(CTeam::Name.eq(team_name))
        .one(&state.web_db)
        .await?
        .or_not_found("Team")?;
    let role = gradient_db::teams::team_role_of(&state.web_db, team.id, user.id).await?;

    match (access, role) {
        (_, None) if !user.superuser => return Err(WebError::not_found("Team")),
        (TeamAccess::Admin { .. }, Some(TeamRole::Member)) if !user.superuser => {
            return Err(WebError::forbidden(
                "You do not have permission to perform this action.",
            ));
        }
        _ => {}
    }

    if let TeamAccess::Admin { reject_managed: true } = access
        && team.managed
    {
        return Err(WebError::forbidden(
            "Cannot modify state-managed team. This team is managed by configuration and cannot be edited through the API.",
        ));
    }

    Ok((team, role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::ApiKeyContext;
    use gradient_db::permissions::mask_from;
    use gradient_db::{WebDb, WorkerDb};
    use gradient_notify::EmailSender;
    use gradient_storage::NarStore;
    use gradient_test_support::cli::test_cli;
    use gradient_test_support::fakes::email::InMemoryEmailSender;
    use gradient_test_support::log_storage::NoopLogStorage;
    use gradient_types::consts::{
        BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
    };
    use gradient_types::ids::{ProjectUserId, RoleId, TaskId};
    use gradient_types::{ConcurrencyPolicy, RuntimeConfig};
    use gradient_types::{ECache, MProjectAccess};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::uuid;

    fn fixture_date() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    fn project_fixture(public: bool, managed: bool) -> gradient_entity::project::Model {
        gradient_entity::project::Model {
            id: ProjectId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            name: "test-project".into(),
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

    fn task_fixture(managed: bool) -> gradient_entity::task::Model {
        gradient_entity::task::Model {
            id: TaskId::new(uuid!("a0000000-0000-0000-0000-000000000002")),
            project: ProjectId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            name: "test-task".into(),
            display_name: "Test".into(),
            repository: "git@example.com:test/test.git".into(),
            wildcard: "*".into(),
            active: true,
            last_check_at: fixture_date(),
            created_by: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
            created_at: fixture_date(),
            managed,
            keep_evaluations: 30,
            concurrency: ConcurrencyPolicy::Skip,
            sign_cache: true,
            ..Default::default()
        }
    }

    fn membership_fixture(role: RoleId) -> gradient_entity::project_user::Model {
        gradient_entity::project_user::Model {
            id: ProjectUserId::new(uuid!("a0000000-0000-0000-0000-000000000010")),
            project: ProjectId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
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

    fn make_state_with_log(db: MockDatabase) -> (Arc<ServerState>, sea_orm::DatabaseConnection) {
        let conn = db.into_connection();
        (make_state(conn.clone()), conn)
    }

    fn make_state(db: sea_orm::DatabaseConnection) -> Arc<ServerState> {
        let cli = test_cli();
        let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
        let nar_storage = NarStore::local(&config.server.base_dir).expect("nar store");
        Arc::new(ServerState {
            web_db: WebDb::new(db),
            cache_db: gradient_db::CacheDb::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
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
            last_used_stamps: gradient_core::last_used_stamps(),
            build_progress: gradient_core::build_progress(),
            eval_progress: gradient_core::eval_progress(),
            cache_traffic: gradient_db::metrics::cache_traffic::CacheTraffic::shared(),
            jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_project_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
            scim_group_roles: std::sync::Arc::new(Default::default()),
            events: gradient_types::EventBus::default(),
            git_host: gradient_git_host::GitHostRegistry::with_builtin(),
            github_app_install_url: Default::default(),
            upstream_query: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            upload_admission: gradient_storage::admission::UploadAdmission::new(
                gradient_storage::admission::Limits {
                    concurrency: 16,
                    bytes: u64::MAX,
                },
            ),
            delivery_wake: Default::default(),
            eval_assign_wake: Default::default(),
            probe_requests: Default::default(),
            held_evaluations: Default::default(),
            startable_set: Default::default(),
            graph: gradient_core::Graph::stub(),
        })
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn admin_required() -> ProjectAccess {
        ProjectAccess::Require {
            permission: Permission::ManageMembers,
            reject_managed: true,
        }
    }

    #[test]
    fn project_admin_passes() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                admin_required(),
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn project_admin_view_role_forbidden() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                admin_required(),
            )
            .await
            .expect_err("view-only must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn project_admin_managed_forbidden() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                admin_required(),
            )
            .await
            .expect_err("managed must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn project_admin_non_member_not_found() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([Vec::<gradient_entity::project_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                admin_required(),
            )
            .await
            .expect_err("non-member must be rejected");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn project_writable_write_role_passes() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Require {
                permission: Permission::ManageActions,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_WRITE_ID)]])
                .append_query_results([vec![write_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                access,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn project_writable_view_role_forbidden() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Require {
                permission: Permission::ManageActions,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                access,
            )
            .await
            .expect_err("view-only must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn project_member_view_role_passes() {
        run(async {
            let user = user_fixture();
            let access = ProjectAccess::Member {
                reject_managed: false,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                access,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn project_readable_public_visible_to_anon() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(true, false)]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state,
                Caller::Anon,
                None,
                "test-project".into(),
                ProjectAccess::Readable { label: "Project" },
            )
            .await;
            assert!(r.is_ok());
        });
    }

    #[test]
    fn project_readable_private_invisible_to_anon() {
        run(async {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state,
                Caller::Anon,
                None,
                "test-project".into(),
                ProjectAccess::Readable { label: "Project" },
            )
            .await
            .expect_err("anon must not see private project");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn task_editable_admin_passes() {
        run(async {
            let user = user_fixture();
            let access = TaskAccess::Require {
                permission: Permission::EditTask,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_task(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                "test-task".into(),
                access,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn task_editable_view_forbidden() {
        run(async {
            let user = user_fixture();
            let access = TaskAccess::Require {
                permission: Permission::EditTask,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                "test-task".into(),
                access,
            )
            .await
            .expect_err("view-only must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn task_editable_managed_forbidden() {
        run(async {
            let user = user_fixture();
            let access = TaskAccess::Require {
                permission: Permission::EditTask,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                "test-task".into(),
                access,
            )
            .await
            .expect_err("managed must be rejected");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn task_missing_returns_task_label() {
        run(async {
            let user = user_fixture();
            let access = TaskAccess::Require {
                permission: Permission::EditTask,
                reject_managed: true,
            };
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<gradient_entity::project::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                "test-task".into(),
                access,
            )
            .await
            .expect_err("missing must be rejected");
            match err {
                WebError::NotFound(_, msg) => assert!(msg.contains("Task"), "got {}", msg),
                other => panic!("expected NotFound, got {:?}", other),
            }
        });
    }

    fn api_key_ctx(mask: PermissionMask, project: Option<ProjectId>) -> ApiKeyContext {
        ApiKeyContext {
            api_id: gradient_entity::ids::ApiId::new(uuid!("a0000000-0000-0000-0000-000000000099")),
            mask,
            project,
            cache_pin: None,
            cache_permission_mask: None,
            allowed_ips: Vec::new(),
        }
    }

    #[test]
    fn api_key_intersection_caps_admin_user_to_view_only() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(Permission::ViewProject.bit(), None);
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let access = ProjectAccess::Require {
                permission: Permission::ManageMembers,
                reject_managed: true,
            };
            let err = load_project(
                &state,
                Caller::User(&user),
                Some(&key),
                "test-project".into(),
                access,
            )
            .await
            .expect_err("admin user must be capped by view-only key");
            assert!(matches!(err, WebError::Forbidden(..)));
        });
    }

    #[test]
    fn api_key_pinned_to_other_project_returns_not_found() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(
                mask_from(Permission::ALL),
                Some(ProjectId::new(uuid!(
                    "a0000000-0000-0000-0000-0000000000ff"
                ))),
            );
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .into_connection();
            let state = make_state(db);
            let access = ProjectAccess::Member {
                reject_managed: false,
            };
            let err = load_project(
                &state,
                Caller::User(&user),
                Some(&key),
                "test-project".into(),
                access,
            )
            .await
            .expect_err("pinned-elsewhere key must be invisible to this project");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn api_key_pinned_to_matching_project_passes() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(
                mask_from(Permission::ALL),
                Some(ProjectId::new(uuid!(
                    "a0000000-0000-0000-0000-000000000001"
                ))),
            );
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let access = ProjectAccess::Require {
                permission: Permission::ManageMembers,
                reject_managed: false,
            };
            let r = load_project(
                &state,
                Caller::User(&user),
                Some(&key),
                "test-project".into(),
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
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state,
                Caller::User(&user),
                None,
                "test-project".into(),
                admin_required(),
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    fn cache_fixture(managed: bool) -> gradient_entity::cache::Model {
        gradient_entity::cache::Model {
            id: gradient_types::ids::CacheId::new(uuid!("a0000000-0000-0000-0000-000000000020")),
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
            cache: gradient_types::ids::CacheId::new(uuid!("a0000000-0000-0000-0000-000000000020")),
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
            project: None,
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

    fn project_cache_fixture() -> gradient_entity::project_cache::Model {
        gradient_entity::project_cache::Model {
            id: gradient_types::ids::ProjectCacheId::new(uuid!(
                "a0000000-0000-0000-0000-000000000040"
            )),
            project: ProjectId::new(uuid!("a0000000-0000-0000-0000-000000000001")),
            cache: gradient_types::ids::CacheId::new(uuid!("a0000000-0000-0000-0000-000000000020")),
            mode: gradient_entity::project_cache::CacheSubscriptionMode::ReadOnly,
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
    fn effective_cache_mask_returns_view_for_project_subscriber() {
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .append_query_results([vec![project_cache_fixture()]])
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
                .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
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

    #[test]
    fn visible_cache_condition_includes_owned_and_subscribed() {
        use sea_orm::QueryTrait;
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![project_cache_fixture()]])
                .into_connection();
            let state = make_state(db);
            let cond = visible_cache_condition(&state, user.id)
                .await
                .expect("condition builds");
            let stmt = ECache::find().filter(cond).build(DatabaseBackend::Postgres);
            assert!(stmt.sql.contains("created_by"), "sql = {}", stmt.sql);
            let values = format!("{:?}", stmt.values);
            assert!(
                values.contains("a0000000-0000-0000-0000-000000000020"),
                "subscribed cache id must feed the filter: {values}"
            );
        });
    }

    #[test]
    fn visible_cache_condition_owner_only_without_memberships() {
        use sea_orm::QueryTrait;
        run(async {
            let user = user_fixture();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<gradient_entity::project_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let cond = visible_cache_condition(&state, user.id)
                .await
                .expect("condition builds");
            let stmt = ECache::find().filter(cond).build(DatabaseBackend::Postgres);
            assert!(stmt.sql.contains("created_by"), "sql = {}", stmt.sql);
        });
    }

    #[tokio::test]
    async fn a_private_project_is_resolved_through_project_access() {
        let project = MProject {
            id: ProjectId::now_v7(),
            name: "private".into(),
            public: false,
            ..Default::default()
        };
        let user = MUser {
            id: UserId::now_v7(),
            ..Default::default()
        };
        let mock = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![project.clone()]])
            .append_query_results([vec![MProjectAccess {
                project: project.id,
                user: user.id,
                role: BASE_ROLE_VIEW_ID,
            }]]);
        let (state, db) = make_state_with_log(mock);

        load_project(
            &state,
            Caller::User(&user),
            None,
            "private".into(),
            ProjectAccess::Readable { label: "Project" },
        )
        .await
        .expect("a team grant is enough to read the project");

        let read_access_view = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .any(|s| s.sql.contains("\"project_access\""));
        assert!(
            read_access_view,
            "visibility must be read from project_access"
        );
    }
}
