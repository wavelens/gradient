/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::permissions::{
    CachePermission, Permission, PermissionMask, cache_mask_grants, cache_mask_to_vec, mask_grants,
    mask_to_vec,
};
use crate::refusal::{Change, Refusal, RefusalStatus, SharedRefusal};
use crate::resource::Resource;
use gradient_db::access::CacheRoleAndPermissionMask;
use gradient_db::lookup::{get_any_cache_by_name, get_any_project_by_name, get_any_task_by_name};
use gradient_db::{DbContext, WebDb};
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_ROLE_ADMIN_ID};
use gradient_types::ids::{CacheId, ExecJobId, ProjectId, TaskId, UserId};
use gradient_types::{
    CProjectUser, EProjectUser, MCache, MEvaluation, MProject, MProjectUser, MTask, MTeam, MUser,
    TeamRole,
};
use sea_orm::Condition;
use sea_orm::sea_query::Expr;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AccessRefusal {
    #[error("Cache-pinned API key cannot be used on this endpoint.")]
    CachePinnedKeyOnEndpoint,
    #[error(
        "An API key pinned to one project can't use this endpoint. This endpoint can show data from all projects."
    )]
    ProjectPinnedKeyOnAllProjectsEndpoint,
    #[error("API key is pinned to a different cache.")]
    KeyPinnedToDifferentCache,
    #[error("Requests to this project need the \"{}\" permission.", .0.permission_label())]
    MissingProjectPermission(Permission),
    #[error("Requests to this endpoint need the \"{}\" permission.", .0.permission_label())]
    MissingServerWidePermission(Permission),
    #[error("Requests to this cache need the \"{}\" permission.", .0.permission_label())]
    MissingCachePermission(CachePermission),
    #[error(
        "Link this upstream cache after granting these permissions on that cache - {}.",
        .0.iter().map(|p| format!("\"{}\"", p.permission_label())).collect::<Vec<_>>().join(", ")
    )]
    MissingUpstreamCacheStorePermissions(Vec<CachePermission>),
    #[error(
        "Users can grant or remove only project permissions that they have. Missing - {}.",
        .0.iter().map(|p| format!("\"{}\"", p.permission_label())).collect::<Vec<_>>().join(", ")
    )]
    MissingProjectPermissions(Vec<Permission>),
    #[error(
        "A caller can grant or revoke only cache permissions that belong to that caller. Missing - {}.",
        .0.iter().map(|p| format!("\"{}\"", p.permission_label())).collect::<Vec<_>>().join(", ")
    )]
    MissingCachePermissions(Vec<CachePermission>),
}

impl Refusal for AccessRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::Forbidden
    }

    fn missing_permission_wire_names(&self) -> Vec<&'static str> {
        match self {
            Self::MissingProjectPermission(permission)
            | Self::MissingServerWidePermission(permission) => vec![permission.as_wire_name()],
            Self::MissingCachePermission(permission) => vec![permission.as_wire_name()],
            Self::MissingUpstreamCacheStorePermissions(permissions)
            | Self::MissingCachePermissions(permissions) => {
                permissions.iter().map(|p| p.as_wire_name()).collect()
            }
            Self::MissingProjectPermissions(permissions) => {
                permissions.iter().map(|p| p.as_wire_name()).collect()
            }
            Self::CachePinnedKeyOnEndpoint
            | Self::ProjectPinnedKeyOnAllProjectsEndpoint
            | Self::KeyPinnedToDifferentCache => Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Caller<'a> {
    Anon,
    User(&'a MUser),
}

impl<'a> From<&'a Option<MUser>> for Caller<'a> {
    fn from(maybe: &'a Option<MUser>) -> Self {
        match maybe {
            Some(u) => Caller::User(u),
            None => Caller::Anon,
        }
    }
}

impl<'a> Caller<'a> {
    pub fn user_id(&self) -> Option<UserId> {
        match self {
            Caller::User(u) => Some(u.id),
            Caller::Anon => None,
        }
    }
}

#[derive(Clone, Copy)]
pub enum ProjectAccess {
    /// Private projects require membership. Task endpoints are passing `Resource::Task` as `label` so a
    /// not-found answer cannot leak that the project exists.
    Readable {
        label: Resource,
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
    ReadStoreAllowed,
    Require {
        permission: CachePermission,
        reject_managed: bool,
    },
    Member {
        reject_managed: bool,
    },
}

pub async fn load_project(
    db: &DbContext,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    project_name: String,
    access: ProjectAccess,
) -> WebResult<MProject> {
    if api_key.is_some_and(|k| k.cache_pin.is_some()) {
        return Err(AccessRefusal::CachePinnedKeyOnEndpoint.into());
    }

    let label = match access {
        ProjectAccess::Readable { label } => label,
        _ => Resource::Project,
    };

    let project = get_any_project_by_name(db, project_name)
        .await?
        .or_not_found(label)?;

    let (requirement, reject_managed) = match access {
        ProjectAccess::Readable { .. } => (
            ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
            false,
        ),
        ProjectAccess::Member { reject_managed } => (
            ProjectItemAccessRequirement::ProjectMemberOnlyViewProject,
            reject_managed,
        ),
        ProjectAccess::Require {
            permission,
            reject_managed,
        } => (
            ProjectItemAccessRequirement::IncludesPermission(permission),
            reject_managed,
        ),
    };
    can_access_project_resource(
        &db.web_db,
        caller,
        api_key,
        &project,
        ProjectItemLocation::ProjectAsWhole,
        requirement,
        label,
    )
    .await?;
    if reject_managed {
        reject_managed_project(&project)?;
    }

    Ok(project)
}

pub async fn load_task(
    db: &DbContext,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    project_name: String,
    task_name: String,
    access: TaskAccess,
) -> WebResult<(MProject, MTask)> {
    if api_key.is_some_and(|k| k.cache_pin.is_some()) {
        return Err(AccessRefusal::CachePinnedKeyOnEndpoint.into());
    }

    let label = Resource::Task;

    let (project, task) = get_any_task_by_name(db, project_name, task_name)
        .await?
        .or_not_found(label)?;

    let (requirement, reject_managed) = match access {
        TaskAccess::Readable => (
            ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
            false,
        ),
        TaskAccess::Member => (
            ProjectItemAccessRequirement::ProjectMemberOnlyViewProject,
            false,
        ),
        TaskAccess::Require {
            permission,
            reject_managed,
        } => (
            ProjectItemAccessRequirement::IncludesPermission(permission),
            reject_managed,
        ),
    };
    let resource = ProjectItemLocation::ItemInTask {
        task: task.id,
        parent_exec_job: None,
    };
    can_access_project_resource(
        &db.web_db,
        caller,
        api_key,
        &project,
        resource,
        requirement,
        label,
    )
    .await?;
    if reject_managed && task.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Task).into());
    }

    Ok((project, task))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectItemAccessRequirement {
    PublicProjectOrCanViewProject,
    ProjectMemberOnlyViewProject,
    IncludesPermission(Permission),
    CanAbortRunningWork,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectItemLocation {
    ProjectAsWhole,
    ItemInTask {
        task: TaskId,
        parent_exec_job: Option<ExecJobId>,
    },
}

impl ProjectItemLocation {
    pub fn from_evaluation(evaluation: &MEvaluation) -> WebResult<Self> {
        let task = evaluation.task.ok_or_else(|| {
            tracing::warn!(evaluation_id = %evaluation.id, "evaluation has no task");
            WebError::data_inconsistency(Resource::Evaluation)
        })?;
        Ok(Self::ItemInTask {
            task,
            parent_exec_job: evaluation.parent_exec_job,
        })
    }
}

/// The one access check for anything in a project. The API key cap and the task binding of a job
/// token apply before any requirement. Private projects also need ViewProject and a caller
/// without it gets the same 404 as for a missing row.
/// An instance admin can bypass project permissions only to abort running work.
pub async fn can_access_project_resource(
    db: &WebDb,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    project: &MProject,
    resource: ProjectItemLocation,
    requirement: ProjectItemAccessRequirement,
    label: Resource,
) -> WebResult<()> {
    use ProjectItemAccessRequirement::*;

    if api_key
        .and_then(|key| key.job_binding)
        .is_some_and(|binding| !binding.allows_resource(resource))
    {
        return Err(WebError::not_found(label));
    }
    match (requirement, caller) {
        (PublicProjectOrCanViewProject, _) if project.public => return Ok(()),
        (CanAbortRunningWork, Caller::User(user)) if user.superuser => return Ok(()),
        _ => {}
    }

    let Some(user_id) = caller.user_id() else {
        return Err(WebError::not_found(label));
    };
    let mask = load_membership_with_permissions(db, user_id, project.id, api_key).await?;
    let can_view = mask.is_some_and(|mask| mask_grants(mask, Permission::ViewProject));
    let permission = match requirement {
        PublicProjectOrCanViewProject | ProjectMemberOnlyViewProject => {
            return if can_view {
                Ok(())
            } else {
                Err(WebError::not_found(label))
            };
        }
        IncludesPermission(permission) => permission,
        CanAbortRunningWork => Permission::TriggerEvaluation,
    };
    if !project.public && !can_view {
        return Err(WebError::not_found(label));
    }
    match mask {
        Some(mask) if mask_grants(mask, permission) => Ok(()),
        _ => Err(AccessRefusal::MissingProjectPermission(permission).into()),
    }
}

pub async fn load_cache(
    db: &DbContext,
    caller: Caller<'_>,
    api_key: Option<&ApiKeyContext>,
    cache_name: String,
    access: CacheAccess,
) -> WebResult<MCache> {
    let label = Resource::Cache;

    let cache = get_any_cache_by_name(db, cache_name)
        .await?
        .or_not_found(label)?;

    if let Some(key) = api_key
        && let Some(pin) = key.cache_pin
        && pin != cache.id
    {
        return Err(AccessRefusal::KeyPinnedToDifferentCache.into());
    }

    match access {
        CacheAccess::ReadStoreAllowed => {
            if !cache.public {
                let allowed = match caller.user_id() {
                    Some(uid) => can_read_store(&db.web_db, uid, cache.id, api_key).await?,
                    None => false,
                };
                if !allowed {
                    return Err(WebError::not_found(label));
                }
            }
        }
        CacheAccess::Readable => {
            if !cache.public {
                let visible = match caller.user_id() {
                    Some(uid) => {
                        can_view_cache(&db.web_db, uid, cache.id, api_key).await?
                            || is_cache_project_subscriber(&db.web_db, uid, cache.id, api_key)
                                .await?
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
            if !can_view_cache(&db.web_db, uid, cache.id, api_key).await? {
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
            require_cache_permission(&db.web_db, uid, cache.id, permission, label, api_key).await?;
            if reject_managed {
                reject_managed_cache(&cache)?;
            }
        }
    }

    Ok(cache)
}

pub fn board_cache_condition(public_listing: bool, user: &Option<MUser>) -> Condition {
    let visible = match user {
        Some(user) if user.superuser => return Condition::all(),
        Some(user) => Some(gradient_db::caches::visible_to_user(user.id)),
        None => None,
    };
    listed_or(public_listing, visible)
}

fn listed_or(public_listing: bool, visible: Option<Condition>) -> Condition {
    match (public_listing, visible) {
        (true, visible) => gradient_db::caches::public_or(visible),
        (false, Some(visible)) => visible,
        (false, None) => Condition::all().add(Expr::val(false)),
    }
}

pub async fn can_view_project(
    db: &WebDb,
    user_id: UserId,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    has_permission(db, user_id, project_id, Permission::ViewProject, api_key).await
}

pub async fn load_project_membership(
    db: &WebDb,
    user_id: UserId,
    project_id: ProjectId,
) -> WebResult<Option<MProjectUser>> {
    Ok(
        gradient_db::memberships::by_owner_and_user::<EProjectUser, _>(
            db,
            CProjectUser::Project,
            project_id,
            CProjectUser::User,
            user_id,
        )
        .await?,
    )
}

pub async fn has_permission(
    db: &WebDb,
    user_id: UserId,
    project_id: ProjectId,
    permission: Permission,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    Ok(
        load_membership_with_permissions(db, user_id, project_id, api_key)
            .await?
            .is_some_and(|mask| mask_grants(mask, permission)),
    )
}

pub async fn load_membership_with_permissions(
    db: &WebDb,
    user_id: UserId,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<PermissionMask>> {
    if api_key_cannot_use_project_permissions(api_key, project_id) {
        return Ok(None);
    }
    Ok(
        gradient_db::access::project_permission_mask(db, project_id, user_id)
            .await?
            .map(|mask| key_limited_project_mask(mask, project_id, api_key)),
    )
}

pub fn api_key_is_pinned_to_other_project(
    api_key: Option<&ApiKeyContext>,
    project_id: ProjectId,
) -> bool {
    api_key.is_some_and(|ctx| ctx.project.is_some_and(|pinned| pinned != project_id))
}

fn api_key_cannot_use_project_permissions(
    api_key: Option<&ApiKeyContext>,
    project_id: ProjectId,
) -> bool {
    api_key.is_some_and(|ctx| ctx.cache_pin.is_some())
        || api_key_is_pinned_to_other_project(api_key, project_id)
}

pub fn key_limited_project_mask(
    mask: PermissionMask,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> PermissionMask {
    match api_key {
        _ if api_key_cannot_use_project_permissions(api_key, project_id) => 0,
        Some(ctx) => mask & ctx.mask,
        None => mask,
    }
}

pub async fn caller_project_mask(
    db: &WebDb,
    caller: Caller<'_>,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<PermissionMask> {
    let Some(user_id) = caller.user_id() else {
        return Ok(0);
    };
    Ok(
        load_membership_with_permissions(db, user_id, project_id, api_key)
            .await?
            .unwrap_or(0),
    )
}

fn reject_managed_project(project: &MProject) -> WebResult<()> {
    if project.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Project).into());
    }
    Ok(())
}

async fn can_view_cache(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    has_cache_permission(db, user_id, cache_id, CachePermission::ViewCache, api_key).await
}

async fn is_cache_project_subscriber(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    if api_key.is_some_and(|key| {
        key.cache_pin.is_some() || !mask_grants(key.mask, Permission::ViewProject)
    }) {
        return Ok(false);
    }

    let subscriber_projects =
        gradient_db::projects::caches::project_ids_with_read_access_to_cache(db, cache_id).await?;

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

    let member = gradient_db::projects::access_of_user_in(db, user_id, allowed).await?;
    Ok(member.is_some())
}

pub async fn effective_cache_mask(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<i64>> {
    Ok(
        effective_cache_role_and_mask(db, user_id, cache_id, api_key)
            .await?
            .map(|grant| grant.permissions),
    )
}

pub async fn effective_cache_role_and_mask(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<CacheRoleAndPermissionMask>> {
    if let Some(grant) =
        cache_member_role_and_permission_mask(db, user_id, cache_id, api_key).await?
    {
        return Ok(Some(grant));
    }
    if is_cache_project_subscriber(db, user_id, cache_id, api_key).await? {
        return Ok(Some(CacheRoleAndPermissionMask {
            role: None,
            permissions: crate::permissions::cache_view_mask(),
        }));
    }
    Ok(None)
}

async fn cache_role_mask(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<i64>> {
    Ok(
        cache_member_role_and_permission_mask(db, user_id, cache_id, api_key)
            .await?
            .map(|grant| grant.permissions),
    )
}

/// An API key can reach the cache through the owner's cache role, unless the key has a pin to a different cache. The cache permission of the key is the cap on that role.
async fn cache_member_role_and_permission_mask(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<CacheRoleAndPermissionMask>> {
    if api_key.is_some_and(|key| {
        key.cache_permission_mask == 0 || key.cache_pin.is_some_and(|pin| pin != cache_id)
    }) {
        return Ok(None);
    }
    Ok(
        gradient_db::access::load_cache_member_role_and_permission_mask(db, cache_id, user_id)
            .await?
            .map(|grant| CacheRoleAndPermissionMask {
                permissions: api_key.map_or(grant.permissions, |key| {
                    grant.permissions & key.cache_permission_mask
                }),
                ..grant
            })
            .filter(|grant| grant.permissions != 0),
    )
}

pub async fn caller_cache_mask(
    db: &WebDb,
    caller: Caller<'_>,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<PermissionMask> {
    let Some(user_id) = caller.user_id() else {
        return Ok(0);
    };
    Ok(cache_role_mask(db, user_id, cache_id, api_key)
        .await?
        .unwrap_or(0))
}

pub(crate) async fn can_read_store(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    Ok(effective_cache_mask(db, user_id, cache_id, api_key)
        .await?
        .is_some_and(|mask| cache_mask_grants(mask, CachePermission::ReadStore)))
}

pub async fn has_cache_permission(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    permission: CachePermission,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<bool> {
    Ok(cache_role_mask(db, user_id, cache_id, api_key)
        .await?
        .is_some_and(|mask| cache_mask_grants(mask, permission)))
}

async fn require_cache_permission(
    db: &WebDb,
    user_id: UserId,
    cache_id: CacheId,
    permission: CachePermission,
    label: Resource,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<()> {
    let mask = cache_role_mask(db, user_id, cache_id, api_key)
        .await?
        .ok_or_else(|| WebError::not_found(label))?;

    if !cache_mask_grants(mask, permission) {
        return Err(AccessRefusal::MissingCachePermission(permission).into());
    }

    Ok(())
}

pub async fn load_caller_project_permissions_and_require_can_grant(
    db: &WebDb,
    caller: Caller<'_>,
    project_id: ProjectId,
    api_key: Option<&ApiKeyContext>,
    granted: PermissionMask,
) -> WebResult<()> {
    let missing = granted & !caller_project_mask(db, caller, project_id, api_key).await?;
    if missing != 0 {
        return Err(AccessRefusal::MissingProjectPermissions(mask_to_vec(missing)).into());
    }
    Ok(())
}

pub async fn ensure_cache_permissions_are_subset_of_caller_permissions(
    db: &WebDb,
    caller: Caller<'_>,
    cache_id: CacheId,
    api_key: Option<&ApiKeyContext>,
    granted: PermissionMask,
) -> WebResult<()> {
    let missing = granted & !caller_cache_mask(db, caller, cache_id, api_key).await?;
    if missing != 0 {
        return Err(AccessRefusal::MissingCachePermissions(cache_mask_to_vec(missing)).into());
    }
    Ok(())
}

pub async fn project_admin_emails(db: &WebDb, project_id: ProjectId) -> WebResult<Vec<String>> {
    let admin_ids: Vec<UserId> =
        gradient_db::projects::members_with_role(db, project_id, BASE_ROLE_ADMIN_ID)
            .await?
            .into_iter()
            .map(|m| m.user)
            .collect();

    admin_emails(db, admin_ids).await
}

pub async fn cache_admin_emails(db: &WebDb, cache_id: CacheId) -> WebResult<Vec<String>> {
    let admin_ids: Vec<UserId> =
        gradient_db::caches::members::with_role(db, cache_id, BASE_CACHE_ROLE_ADMIN_ID)
            .await?
            .into_iter()
            .map(|m| m.user)
            .collect();

    admin_emails(db, admin_ids).await
}

async fn admin_emails(db: &WebDb, ids: Vec<UserId>) -> WebResult<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    Ok(gradient_db::users::with_ids(db, &ids)
        .await?
        .into_iter()
        .filter(|u| u.active && !u.email.is_empty())
        .map(|u| u.email)
        .collect())
}

pub(crate) fn reject_managed_cache(cache: &MCache) -> WebResult<()> {
    if cache.managed {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Cache).into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub enum TeamAccess {
    Member,
    Admin { reject_managed: bool },
}

pub async fn load_team(
    db: &WebDb,
    user: &MUser,
    api_key: Option<&ApiKeyContext>,
    team_name: String,
    access: TeamAccess,
) -> WebResult<(MTeam, Option<TeamRole>)> {
    crate::authorization::reject_api_key_authentication(api_key)?;

    let team = gradient_db::teams::by_name(db, &team_name)
        .await?
        .or_not_found(Resource::Team)?;
    let role = gradient_db::teams::team_role_of(db, team.id, user.id).await?;

    match (access, role) {
        (_, None) if !user.superuser => return Err(WebError::not_found(Resource::Team)),
        (TeamAccess::Admin { .. }, Some(TeamRole::Member)) if !user.superuser => {
            return Err(SharedRefusal::MissingPermission.into());
        }
        _ => {}
    }

    if let TeamAccess::Admin {
        reject_managed: true,
    } = access
        && team.managed
    {
        return Err(SharedRefusal::StateManaged(Change::Modify, Resource::Team).into());
    }

    Ok((team, role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::ApiKeyContext;
    use gradient_core::ServerState;
    use std::sync::Arc;

    use gradient_fixtures::mock::{all_statements, mock_db};

    use gradient_test_support::state::TestState;
    use gradient_types::ConcurrencyPolicy;
    use gradient_types::consts::{
        BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
    };
    use gradient_types::ids::{ProjectUserId, RoleId, TaskId};
    use gradient_types::permissions::mask_from;
    use gradient_types::{ECache, MProjectAccess};
    use sea_orm::{DatabaseBackend, EntityTrait, MockDatabase, QueryFilter};
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
        TestState::new().web_db(db).build()
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([Vec::<gradient_entity::project_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_WRITE_ID)]])
                .append_query_results([vec![write_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(true, false)]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state.db(),
                Caller::Anon,
                None,
                "test-project".into(),
                ProjectAccess::Readable {
                    label: Resource::Project,
                },
            )
            .await;
            assert!(r.is_ok());
        });
    }

    #[test]
    fn project_readable_private_invisible_to_anon() {
        run(async {
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state.db(),
                Caller::Anon,
                None,
                "test-project".into(),
                ProjectAccess::Readable {
                    label: Resource::Project,
                },
            )
            .await
            .expect_err("anon must not see private project");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn project_readable_role_without_view_project_not_found() {
        run(async {
            let user = user_fixture();
            let role_id = RoleId::new(uuid!("a0000000-0000-0000-0000-000000000040"));
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(role_id)]])
                .append_query_results([vec![role_fixture(
                    role_id,
                    mask_from(&[Permission::ManageWorkers]),
                )]])
                .into_connection();
            let state = make_state(db);
            let err = load_project(
                &state.db(),
                Caller::User(&user),
                None,
                "test-project".into(),
                ProjectAccess::Readable {
                    label: Resource::Project,
                },
            )
            .await
            .expect_err("a role without viewProject must not read the project");
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_task(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .append_query_results([vec![view_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state.db(),
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
    fn api_key_without_trigger_evaluation_permission_is_refused_on_project_action_with_403_and_missing_permission()
     {
        run(async {
            let user = user_fixture();
            let api_key = api_key_ctx(mask_from(&[Permission::ViewProject]), None);
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-project".into(),
                "test-task".into(),
                TaskAccess::Require {
                    permission: Permission::TriggerEvaluation,
                    reject_managed: false,
                },
            )
            .await
            .expect_err("the key lacks triggerEvaluation");
            assert!(
                matches!(&err, WebError::Forbidden(_, _, missing) if missing == &["triggerEvaluation"]),
                "{err:?}"
            );
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![task_fixture(true)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([Vec::<gradient_entity::project::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_task(
                &state.db(),
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
            mask,
            cache_permission_mask: 0,
            project,
            cache_pin: None,
            allowed_ips: Vec::new(),
            job_binding: None,
        }
    }

    fn job_token_ctx(task: TaskId, exec_job: ExecJobId) -> ApiKeyContext {
        ApiKeyContext {
            job_binding: Some(crate::authorization::JobTokenTaskAndExecJobIds { task, exec_job }),
            ..api_key_ctx(
                mask_from(Permission::ALL),
                Some(project_fixture(true, false).id),
            )
        }
    }

    #[test]
    fn job_token_cannot_read_item_in_other_task_of_same_project() {
        run(async {
            let user = user_fixture();
            let key = job_token_ctx(TaskId::now_v7(), ExecJobId::now_v7());
            let state = make_state(mock_db().into_connection());
            let resource = ProjectItemLocation::ItemInTask {
                task: TaskId::now_v7(),
                parent_exec_job: None,
            };
            let err = can_access_project_resource(
                &state.web_db,
                Caller::User(&user),
                Some(&key),
                &project_fixture(true, false),
                resource,
                ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
                Resource::Evaluation,
            )
            .await
            .expect_err("a job token is bound to its own task");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn job_token_can_read_evaluation_started_by_its_own_exec_job() {
        run(async {
            let user = user_fixture();
            let exec_job = ExecJobId::now_v7();
            let key = job_token_ctx(TaskId::now_v7(), exec_job);
            let state = make_state(mock_db().into_connection());
            let resource = ProjectItemLocation::ItemInTask {
                task: TaskId::now_v7(),
                parent_exec_job: Some(exec_job),
            };
            let result = can_access_project_resource(
                &state.web_db,
                Caller::User(&user),
                Some(&key),
                &project_fixture(true, false),
                resource,
                ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
                Resource::Evaluation,
            )
            .await;
            assert!(result.is_ok(), "{:?}", result.err());
        });
    }

    #[test]
    fn superuser_without_project_role_gets_404_for_trigger_evaluation_but_can_abort() {
        run(async {
            let user = MUser {
                superuser: true,
                ..user_fixture()
            };
            let db = mock_db()
                .append_query_results([Vec::<gradient_entity::project_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let project = project_fixture(false, false);
            let check = |requirement| {
                can_access_project_resource(
                    &state.web_db,
                    Caller::User(&user),
                    None,
                    &project,
                    ProjectItemLocation::ProjectAsWhole,
                    requirement,
                    Resource::ExecJob,
                )
            };
            let err = check(ProjectItemAccessRequirement::IncludesPermission(
                Permission::TriggerEvaluation,
            ))
            .await
            .expect_err("a superuser holds no project permission");
            assert!(matches!(err, WebError::NotFound(..)));
            check(ProjectItemAccessRequirement::CanAbortRunningWork)
                .await
                .expect("a superuser may stop running work");
        });
    }

    fn check_project_access_for_api_key_with_only_trigger_evaluation_permission(
        public: bool,
        requirement: ProjectItemAccessRequirement,
    ) -> WebResult<()> {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(Permission::TriggerEvaluation.bit(), None);
            let db = mock_db()
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            can_access_project_resource(
                &state.web_db,
                Caller::User(&user),
                Some(&key),
                &project_fixture(public, false),
                ProjectItemLocation::ProjectAsWhole,
                requirement,
                Resource::ExecJob,
            )
            .await
        })
    }

    #[test]
    fn api_key_without_view_project_gets_not_found_for_every_action_on_private_project() {
        for requirement in [
            ProjectItemAccessRequirement::IncludesPermission(Permission::TriggerEvaluation),
            ProjectItemAccessRequirement::CanAbortRunningWork,
        ] {
            let err = check_project_access_for_api_key_with_only_trigger_evaluation_permission(
                false,
                requirement,
            )
            .expect_err("Private projects require ViewProject for all actions.");
            assert!(matches!(err, WebError::NotFound(..)), "{requirement:?}");
        }
    }

    #[test]
    fn public_project_action_does_not_need_view_project_permission() {
        check_project_access_for_api_key_with_only_trigger_evaluation_permission(
            true,
            ProjectItemAccessRequirement::IncludesPermission(Permission::TriggerEvaluation),
        )
        .expect("Public projects are visible without ViewProject.");
    }

    #[test]
    fn a_cache_pinned_key_holds_no_project_permission() {
        run(async {
            let key = ApiKeyContext {
                cache_pin: Some(CacheId::now_v7()),
                ..api_key_ctx(mask_from(Permission::ALL), None)
            };
            let project = ProjectId::now_v7();
            let state = make_state(mock_db().into_connection());

            assert_eq!(
                key_limited_project_mask(mask_from(Permission::ALL), project, Some(&key)),
                0
            );
            assert!(
                !can_view_project(&state.web_db, UserId::now_v7(), project, Some(&key))
                    .await
                    .expect("no query runs for a cache-pinned key")
            );
        });
    }

    #[test]
    fn api_key_intersection_caps_admin_user_to_view_only() {
        run(async {
            let user = user_fixture();
            let key = api_key_ctx(Permission::ViewProject.bit(), None);
            let db = mock_db()
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
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .into_connection();
            let state = make_state(db);
            let access = ProjectAccess::Member {
                reject_managed: false,
            };
            let err = load_project(
                &state.db(),
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
            let db = mock_db()
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
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![project_fixture(false, false)]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_ADMIN_ID)]])
                .append_query_results([vec![admin_role_row()]])
                .into_connection();
            let state = make_state(db);
            let r = load_project(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![cache_fixture(true)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![cache_fixture(true)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
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

    fn cache_view_api_key() -> ApiKeyContext {
        ApiKeyContext {
            mask: 0,
            cache_permission_mask: crate::permissions::cache_view_mask(),
            project: None,
            cache_pin: Some(cache_fixture(false).id),
            allowed_ips: Vec::new(),
            job_binding: None,
        }
    }

    #[test]
    fn cache_readable_allows_public_anon() {
        run(async {
            let db = mock_db()
                .append_query_results([vec![cache_fixture_public(false)]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
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
    fn cache_readable_role_without_view_cache_not_found() {
        run(async {
            let user = user_fixture();
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![gradient_entity::cache_role::Model {
                    permission: 0,
                    ..cache_role_view_fixture()
                }]])
                .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
                Caller::User(&user),
                None,
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await
            .expect_err("a role without viewCache must not read the cache");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn api_key_with_only_view_cache_permission_cannot_open_private_cache_read_connection() {
        run(async {
            let user = user_fixture();
            let key = ApiKeyContext {
                cache_permission_mask: gradient_types::permissions::cache_mask_from(&[
                    CachePermission::ViewCache,
                ]),
                ..cache_view_api_key()
            };
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_view_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&key),
                "test-cache".into(),
                CacheAccess::ReadStoreAllowed,
            )
            .await
            .expect_err("a key without readStore must not download from the cache");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn cache_member_allows_member() {
        run(async {
            let user = user_fixture();
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state.db(),
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
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_view_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
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

    fn project_id_only_mock_row() -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        std::collections::BTreeMap::from([(
            "project",
            membership_fixture(BASE_ROLE_VIEW_ID).project.into(),
        )])
    }

    #[test]
    fn effective_cache_mask_returns_role_for_member() {
        run(async {
            let user = user_fixture();
            let db = mock_db()
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let access = effective_cache_role_and_mask(
                &state.web_db,
                user.id,
                cache_fixture(false).id,
                None,
            )
            .await
            .expect("query ok")
            .expect("member has a mask");
            assert_eq!(access.permissions, crate::permissions::cache_admin_mask());
            assert_eq!(access.role.as_deref(), Some("Admin"));
        });
    }

    #[test]
    fn effective_cache_mask_returns_view_for_project_subscriber() {
        run(async {
            let user = user_fixture();
            let db = mock_db()
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .append_query_results([vec![project_id_only_mock_row()]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let access = effective_cache_role_and_mask(
                &state.web_db,
                user.id,
                cache_fixture(false).id,
                None,
            )
            .await
            .expect("query ok")
            .expect("subscriber gets view-only mask");
            assert_eq!(access.permissions, crate::permissions::cache_view_mask());
            assert_eq!(access.role, None);
        });
    }

    #[test]
    fn effective_cache_mask_none_for_outsider() {
        run(async {
            let user = user_fixture();
            let db = mock_db()
                .append_query_results([Vec::<gradient_entity::cache_user::Model>::new()])
                .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let mask = effective_cache_mask(&state.web_db, user.id, cache_fixture(false).id, None)
                .await
                .expect("query ok");
            assert!(mask.is_none(), "outsider must not get a cache mask");
        });
    }

    #[test]
    fn cache_require_intersects_with_api_key_mask() {
        run(async {
            let user = user_fixture();
            let api_key = cache_view_api_key();
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
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

    fn api_key_context_for_fixed_test_project() -> ApiKeyContext {
        ApiKeyContext {
            mask: i64::MAX,
            cache_permission_mask: 0,
            project: Some(ProjectId::new(uuid!(
                "a0000000-0000-0000-0000-000000000001"
            ))),
            cache_pin: None,
            allowed_ips: Vec::new(),
            job_binding: None,
        }
    }

    #[test]
    fn unpinned_view_only_api_key_caps_owner_admin_cache_role_to_view_only() {
        run(async {
            let user = user_fixture();
            let api_key = ApiKeyContext {
                cache_permission_mask: crate::permissions::cache_view_mask(),
                ..api_key_ctx(mask_from(&[Permission::ViewProject]), None)
            };
            let db = mock_db()
                .append_query_results([vec![cache_member_fixture()]])
                .append_query_results([vec![cache_role_fixture()]])
                .into_connection();
            let state = make_state(db);
            let access = effective_cache_role_and_mask(
                &state.web_db,
                user.id,
                cache_fixture(false).id,
                Some(&api_key),
            )
            .await
            .expect("query ok")
            .expect("the owner's role reaches the cache through the key");
            assert_eq!(access.permissions, crate::permissions::cache_view_mask());
        });
    }

    #[test]
    fn any_project_key_cannot_change_a_cache() {
        run(async {
            let user = user_fixture();
            let api_key = api_key_ctx(i64::MAX, None);
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                CacheAccess::Require {
                    permission: CachePermission::ManageCacheSettings,
                    reject_managed: false,
                },
            )
            .await
            .expect_err("an any-project key has no cache role");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn key_without_view_project_cannot_read_a_subscribed_cache() {
        run(async {
            let user = user_fixture();
            let api_key = api_key_ctx(mask_from(&[Permission::TriggerEvaluation]), None);
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await
            .expect_err("reading through a project subscription needs View project on the key");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn any_project_key_with_view_project_reads_a_subscribed_cache() {
        run(async {
            let user = user_fixture();
            let api_key = api_key_ctx(mask_from(&[Permission::ViewProject]), None);
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![project_id_only_mock_row()]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn project_pinned_key_ignores_direct_cache_membership() {
        run(async {
            let user = user_fixture();
            let api_key = api_key_context_for_fixed_test_project();
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await
            .expect_err("a project-pinned key must not reach the owner's private caches");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn project_pinned_key_reads_a_cache_its_project_subscribes_to() {
        run(async {
            let user = user_fixture();
            let api_key = api_key_context_for_fixed_test_project();
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .append_query_results([vec![project_id_only_mock_row()]])
                .append_query_results([vec![membership_fixture(BASE_ROLE_VIEW_ID)]])
                .into_connection();
            let state = make_state(db);
            let r = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                CacheAccess::Readable,
            )
            .await;
            assert!(r.is_ok(), "{:?}", r.err());
        });
    }

    #[test]
    fn project_pinned_key_cannot_write_to_a_cache() {
        run(async {
            let user = user_fixture();
            let api_key = api_key_context_for_fixed_test_project();
            let db = mock_db()
                .append_query_results([vec![cache_fixture(false)]])
                .into_connection();
            let state = make_state(db);
            let err = load_cache(
                &state.db(),
                Caller::User(&user),
                Some(&api_key),
                "test-cache".into(),
                write_store_access(),
            )
            .await
            .expect_err("a project-pinned key has no cache role");
            assert!(matches!(err, WebError::NotFound(..)));
        });
    }

    #[test]
    fn cache_visibility_filter_covers_direct_and_project_caches_but_not_the_creator() {
        let visible = gradient_db::caches::visible_to_user(UserId::now_v7());
        let sql = listed_sql(false, Some(visible));
        for name in ["cache_access", "project_access", "project_cache"] {
            assert!(sql.contains(name), "{name} missing from sql = {sql}");
        }
        assert!(!sql.contains("created_by"), "sql = {sql}");
    }

    fn listed_sql(public_listing: bool, visible: Option<Condition>) -> String {
        use sea_orm::QueryTrait;
        let sql = ECache::find()
            .filter(listed_or(public_listing, visible))
            .build(DatabaseBackend::Postgres)
            .to_string();
        let (_, condition) = sql.split_once(" WHERE ").expect("a WHERE clause");
        condition.to_owned()
    }

    #[test]
    fn listing_on_adds_public_caches_to_the_visible_ones() {
        let visible = gradient_db::caches::visible_to_user(UserId::now_v7());
        let sql = listed_sql(true, Some(visible));
        assert!(sql.contains("\"public\" = TRUE"), "sql = {sql}");
        assert!(!sql.contains("created_by"), "sql = {sql}");
    }

    #[test]
    fn listing_off_keeps_only_the_visible_caches() {
        let visible = gradient_db::caches::visible_to_user(UserId::now_v7());
        let sql = listed_sql(false, Some(visible));
        assert!(!sql.contains("\"public\""), "sql = {sql}");
        assert!(!sql.contains("created_by"), "sql = {sql}");
    }

    #[test]
    fn listing_off_shows_an_anonymous_caller_no_cache() {
        let sql = listed_sql(false, None);
        assert_eq!(sql, "FALSE");
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
        let mock = mock_db()
            .append_query_results([vec![project.clone()]])
            .append_query_results([vec![MProjectAccess {
                team: None,
                project: project.id,
                user: user.id,
                role: BASE_ROLE_VIEW_ID,
            }]])
            .append_query_results([vec![view_role_row()]]);
        let (state, db) = make_state_with_log(mock);

        load_project(
            &state.db(),
            Caller::User(&user),
            None,
            "private".into(),
            ProjectAccess::Readable {
                label: Resource::Project,
            },
        )
        .await
        .expect("a team grant is enough to read the project");

        let read_access_view = all_statements(db.into_transaction_log())
            .into_iter()
            .any(|s| s.sql.contains("\"project_access\""));
        assert!(
            read_access_view,
            "visibility must be read from project_access"
        );
    }

    async fn insert_user_private_cache_project_member_and_subscription(
        db: &sea_orm::DatabaseConnection,
        cache_active: bool,
        mode: gradient_entity::project_cache::CacheSubscriptionMode,
    ) -> MUser {
        use gradient_fixtures::rows;
        let user = gradient_db::rows::insert::<gradient_types::EUser, _>(db, rows::user())
            .await
            .expect("Insert a user.");
        gradient_db::rows::insert::<ECache, _>(
            db,
            gradient_entity::cache::Model {
                active: cache_active,
                public: false,
                ..rows::cache()
            },
        )
        .await
        .expect("The cache write succeeded.");
        let project = gradient_db::rows::insert::<gradient_types::EProject, _>(db, rows::project())
            .await
            .expect("The write of the project row finished.");
        gradient_db::rows::insert::<gradient_types::ERole, _>(db, rows::view_role())
            .await
            .expect("The role was written");
        gradient_db::rows::insert::<gradient_types::EProjectUser, _>(
            db,
            rows::project_member(BASE_ROLE_VIEW_ID),
        )
        .await
        .expect("The member row write succeeds.");
        gradient_db::rows::insert::<gradient_types::EProjectCache, _>(
            db,
            rows::project_cache_link(
                gradient_types::ids::ProjectCacheId::now_v7(),
                project.id,
                rows::cache().id,
                mode,
            ),
        )
        .await
        .expect("The subscription write completed.");
        user
    }

    async fn load_cache_readable_and_permission_mask_for_user(
        db: &sea_orm::DatabaseConnection,
        user: &MUser,
    ) -> (bool, Option<i64>) {
        let state = make_state(db.clone());
        let readable = load_cache(
            &state.db(),
            Caller::User(user),
            None,
            gradient_fixtures::rows::cache().name,
            CacheAccess::Readable,
        )
        .await
        .is_ok();
        let mask = effective_cache_mask(
            &state.web_db,
            user.id,
            gradient_fixtures::rows::cache().id,
            None,
        )
        .await
        .expect("Value read from the mask");
        (readable, mask)
    }

    #[tokio::test]
    async fn write_only_private_cache_subscription_gives_project_member_no_cache_permission_mask_and_no_readable_access()
     {
        let test_db = gradient_fixtures::database::TestDatabase::create().await;
        let db = test_db.connection();
        let user = insert_user_private_cache_project_member_and_subscription(
            &db,
            true,
            gradient_entity::project_cache::CacheSubscriptionMode::WriteOnly,
        )
        .await;

        let cache_readable_and_effective_permission_mask =
            load_cache_readable_and_permission_mask_for_user(&db, &user).await;

        assert_eq!(cache_readable_and_effective_permission_mask, (false, None));
        test_db.finish().await;
    }

    #[tokio::test]
    async fn test_read_write_project_member_gets_no_permissions_for_inactive_private_cache() {
        let test_db = gradient_fixtures::database::TestDatabase::create().await;
        let db = test_db.connection();
        let user = insert_user_private_cache_project_member_and_subscription(
            &db,
            false,
            gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
        )
        .await;

        let cache_readable_and_effective_permission_mask =
            load_cache_readable_and_permission_mask_for_user(&db, &user).await;

        assert_eq!(cache_readable_and_effective_permission_mask, (false, None));
        test_db.finish().await;
    }

    #[tokio::test]
    async fn test_project_member_with_read_only_or_read_write_project_subscription_gets_view_cache_and_read_store_mask_and_can_open_active_private_cache_as_readable()
     {
        for mode in [
            gradient_entity::project_cache::CacheSubscriptionMode::ReadOnly,
            gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
        ] {
            let test_db = gradient_fixtures::database::TestDatabase::create().await;
            let db = test_db.connection();
            let user =
                insert_user_private_cache_project_member_and_subscription(&db, true, mode.clone())
                    .await;

            let cache_readable_and_effective_permission_mask =
                load_cache_readable_and_permission_mask_for_user(&db, &user).await;

            assert_eq!(
                cache_readable_and_effective_permission_mask,
                (true, Some(crate::permissions::cache_view_mask())),
                "{mode:?}"
            );
            test_db.finish().await;
        }
    }

    #[tokio::test]
    async fn project_member_with_write_only_subscription_and_direct_cache_write_role_keeps_role_permissions()
     {
        let test_db = gradient_fixtures::database::TestDatabase::create().await;
        let db = test_db.connection();
        let user = insert_user_private_cache_project_member_and_subscription(
            &db,
            true,
            gradient_entity::project_cache::CacheSubscriptionMode::WriteOnly,
        )
        .await;
        gradient_db::rows::insert::<gradient_types::ECacheUser, _>(
            &db,
            gradient_fixtures::rows::cache_member(gradient_types::consts::BASE_CACHE_ROLE_WRITE_ID),
        )
        .await
        .expect("Cache member written");

        let cache_readable_and_effective_permission_mask =
            load_cache_readable_and_permission_mask_for_user(&db, &user).await;

        assert_eq!(
            cache_readable_and_effective_permission_mask,
            (true, Some(crate::permissions::cache_write_mask()))
        );
        test_db.finish().await;
    }

    #[tokio::test]
    async fn project_member_with_read_write_subscription_to_inactive_private_cache_and_direct_admin_role_keeps_cache_admin_permissions()
     {
        let test_db = gradient_fixtures::database::TestDatabase::create().await;
        let db = test_db.connection();
        let user = insert_user_private_cache_project_member_and_subscription(
            &db,
            false,
            gradient_entity::project_cache::CacheSubscriptionMode::ReadWrite,
        )
        .await;
        gradient_db::rows::insert::<gradient_types::ECacheUser, _>(
            &db,
            gradient_fixtures::rows::cache_member(gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID),
        )
        .await
        .expect("Wrote member as cache admin");

        let cache_readable_and_effective_permission_mask =
            load_cache_readable_and_permission_mask_for_user(&db, &user).await;
        let cache_load_with_manage_cache_settings_permission_succeeded = load_cache(
            &make_state(db.clone()).db(),
            Caller::User(&user),
            None,
            gradient_fixtures::rows::cache().name,
            CacheAccess::Require {
                permission: CachePermission::ManageCacheSettings,
                reject_managed: false,
            },
        )
        .await
        .is_ok();

        assert!(
            matches!(
                cache_readable_and_effective_permission_mask,
                (true, Some(mask)) if cache_mask_grants(mask, CachePermission::ManageCacheSettings)
            ),
            "{cache_readable_and_effective_permission_mask:?}"
        );
        assert!(cache_load_with_manage_cache_settings_permission_succeeded);
        test_db.finish().await;
    }
}
