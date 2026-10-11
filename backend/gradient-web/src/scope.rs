/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{
    CacheAccess, Caller, ProjectAccess, TaskAccess, TeamAccess, load_cache, load_project,
    load_task, load_team,
};
use crate::authorization::{ApiKeyContext, MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::permissions::{CachePermission, Permission};
use crate::refusal::{Refusal, RefusalStatus};
use crate::resource::Resource;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_types::{MCache, MProject, MTask, MTeam, MUser, TeamRole};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScopeRefusal {
    #[error("invalid {name}")]
    InvalidParameter { name: String },
    #[error("{0}")]
    InvalidPathParameters(String),
}

impl Refusal for ScopeRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::BadRequest
    }
}

#[derive(Clone, Debug)]
pub struct PathParams(HashMap<String, String>);

impl PathParams {
    pub(crate) async fn from_parts<S: Send + Sync>(
        parts: &mut Parts,
        state: &S,
    ) -> WebResult<Self> {
        let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|e| ScopeRefusal::InvalidPathParameters(e.to_string()))?;
        Ok(Self(params))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub fn param<T: FromStr>(&self, name: &str) -> WebResult<T> {
        self.0
            .get(name)
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                WebError::from(ScopeRefusal::InvalidParameter {
                    name: name.to_string(),
                })
            })
    }
}

fn extension<T: Clone + Send + Sync + 'static>(parts: &Parts) -> WebResult<T> {
    parts.extensions.get::<T>().cloned().ok_or_else(|| {
        WebError::Internal(anyhow::anyhow!(
            "request extension `{}` is missing",
            std::any::type_name::<T>()
        ))
    })
}

pub trait RequestUser: Send + Sync + Sized {
    fn from_parts(parts: &Parts) -> WebResult<Self>;
    fn caller(&self) -> Caller<'_>;
}

impl RequestUser for MUser {
    fn from_parts(parts: &Parts) -> WebResult<Self> {
        extension(parts)
    }

    fn caller(&self) -> Caller<'_> {
        Caller::User(self)
    }
}

impl RequestUser for Option<MUser> {
    fn from_parts(parts: &Parts) -> WebResult<Self> {
        Ok(extension::<MaybeUser>(parts)?.0)
    }

    fn caller(&self) -> Caller<'_> {
        Caller::from(self)
    }
}

struct Incoming<U> {
    user: U,
    api_key: Option<ApiKeyContext>,
    params: PathParams,
    db: DbContext,
}

impl<U: RequestUser> Incoming<U> {
    async fn read(parts: &mut Parts, state: &Arc<ServerState>) -> WebResult<Self> {
        let user = U::from_parts(parts)?;
        let api_key = extension::<MaybeApiKey>(parts)?.0;
        let params = PathParams::from_parts(parts, state).await?;
        Ok(Self {
            user,
            api_key,
            params,
            db: state.db(),
        })
    }
}

pub trait RequiredPermission: Send + Sync + 'static {
    const PERMISSION: Permission;
}

pub trait RequiredCachePermission: Send + Sync + 'static {
    const PERMISSION: CachePermission;
}

macro_rules! permission_markers {
    ($enum:ident, $required:ident, [$($variant:ident),* $(,)?]) => {
        $(
            pub struct $variant;

            impl $required for $variant {
                const PERMISSION: $enum = $enum::$variant;
            }
        )*

        const _: fn($enum) = |permission| match permission {
            $($enum::$variant => (),)*
        };
    };
}

pub mod can {
    use super::{RequiredCachePermission, RequiredPermission};
    use crate::permissions::{CachePermission, Permission};

    permission_markers!(
        Permission,
        RequiredPermission,
        [
            ViewProject,
            ManageProjectSettings,
            DeleteProject,
            ManageMembers,
            ManageRoles,
            ManageIntegrations,
            ManageActions,
            ManageWorkers,
            ManageSubscriptions,
            ManageSshKey,
            CreateTask,
            EditTask,
            TriggerEvaluation,
            ManageTriggers,
            ManageWebhooks,
        ]
    );

    permission_markers!(
        CachePermission,
        RequiredCachePermission,
        [
            ViewCache,
            ReadStore,
            WriteStore,
            ManageCacheSettings,
            ManageCacheKeys,
            ManageUpstreamCaches,
            ManageCacheMembers,
            ManageCacheRoles,
            ManageCacheSubscriptions,
            DeleteCache,
            ManageCacheWebhooks,
        ]
    );
}

pub struct Readable;
pub struct Member;
pub struct Admin;
pub struct AdminUnmanaged;
pub struct Requires<P>(PhantomData<P>);
pub struct RequiresUnmanaged<P>(PhantomData<P>);

pub trait ProjectRule: Send + Sync + 'static {
    type User: RequestUser;
    const ACCESS: ProjectAccess;
}

impl ProjectRule for Readable {
    type User = Option<MUser>;
    const ACCESS: ProjectAccess = ProjectAccess::Readable {
        label: Resource::Project,
    };
}

impl ProjectRule for Member {
    type User = MUser;
    const ACCESS: ProjectAccess = ProjectAccess::Member {
        reject_managed: false,
    };
}

impl<P: RequiredPermission> ProjectRule for Requires<P> {
    type User = MUser;
    const ACCESS: ProjectAccess = ProjectAccess::Require {
        permission: P::PERMISSION,
        reject_managed: false,
    };
}

impl<P: RequiredPermission> ProjectRule for RequiresUnmanaged<P> {
    type User = MUser;
    const ACCESS: ProjectAccess = ProjectAccess::Require {
        permission: P::PERMISSION,
        reject_managed: true,
    };
}

pub trait TaskRule: Send + Sync + 'static {
    type User: RequestUser;
    const ACCESS: TaskAccess;
}

impl TaskRule for Readable {
    type User = Option<MUser>;
    const ACCESS: TaskAccess = TaskAccess::Readable;
}

impl TaskRule for Member {
    type User = MUser;
    const ACCESS: TaskAccess = TaskAccess::Member;
}

impl<P: RequiredPermission> TaskRule for Requires<P> {
    type User = MUser;
    const ACCESS: TaskAccess = TaskAccess::Require {
        permission: P::PERMISSION,
        reject_managed: false,
    };
}

impl<P: RequiredPermission> TaskRule for RequiresUnmanaged<P> {
    type User = MUser;
    const ACCESS: TaskAccess = TaskAccess::Require {
        permission: P::PERMISSION,
        reject_managed: true,
    };
}

pub trait CacheRule: Send + Sync + 'static {
    type User: RequestUser;
    const ACCESS: CacheAccess;
}

impl CacheRule for Readable {
    type User = Option<MUser>;
    const ACCESS: CacheAccess = CacheAccess::Readable;
}

impl CacheRule for Member {
    type User = MUser;
    const ACCESS: CacheAccess = CacheAccess::Member {
        reject_managed: false,
    };
}

impl<P: RequiredCachePermission> CacheRule for Requires<P> {
    type User = MUser;
    const ACCESS: CacheAccess = CacheAccess::Require {
        permission: P::PERMISSION,
        reject_managed: false,
    };
}

impl<P: RequiredCachePermission> CacheRule for RequiresUnmanaged<P> {
    type User = MUser;
    const ACCESS: CacheAccess = CacheAccess::Require {
        permission: P::PERMISSION,
        reject_managed: true,
    };
}

pub trait TeamRule: Send + Sync + 'static {
    const ACCESS: TeamAccess;
}

impl TeamRule for Member {
    const ACCESS: TeamAccess = TeamAccess::Member;
}

impl TeamRule for Admin {
    const ACCESS: TeamAccess = TeamAccess::Admin {
        reject_managed: false,
    };
}

impl TeamRule for AdminUnmanaged {
    const ACCESS: TeamAccess = TeamAccess::Admin {
        reject_managed: true,
    };
}

pub struct ProjectScope<R: ProjectRule> {
    pub project: MProject,
    pub user: R::User,
    pub api_key: Option<ApiKeyContext>,
    pub db: DbContext,
    pub params: PathParams,
    rule: PhantomData<R>,
}

impl<R: ProjectRule> FromRequestParts<Arc<ServerState>> for ProjectScope<R> {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<ServerState>) -> WebResult<Self> {
        let Incoming {
            user,
            api_key,
            params,
            db,
        } = Incoming::<R::User>::read(parts, state).await?;
        let project = load_project(
            &db,
            user.caller(),
            api_key.as_ref(),
            params.param("project")?,
            R::ACCESS,
        )
        .await?;
        Ok(Self {
            project,
            user,
            api_key,
            db,
            params,
            rule: PhantomData,
        })
    }
}

pub struct TaskScope<R: TaskRule> {
    pub project: MProject,
    pub task: MTask,
    pub user: R::User,
    pub api_key: Option<ApiKeyContext>,
    pub db: DbContext,
    pub params: PathParams,
    rule: PhantomData<R>,
}

impl<R: TaskRule> FromRequestParts<Arc<ServerState>> for TaskScope<R> {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<ServerState>) -> WebResult<Self> {
        let Incoming {
            user,
            api_key,
            params,
            db,
        } = Incoming::<R::User>::read(parts, state).await?;
        let (project, task) = load_task(
            &db,
            user.caller(),
            api_key.as_ref(),
            params.param("project")?,
            params.param("task")?,
            R::ACCESS,
        )
        .await?;
        Ok(Self {
            project,
            task,
            user,
            api_key,
            db,
            params,
            rule: PhantomData,
        })
    }
}

pub struct CacheScope<R: CacheRule> {
    pub cache: MCache,
    pub user: R::User,
    pub api_key: Option<ApiKeyContext>,
    pub db: DbContext,
    pub params: PathParams,
    rule: PhantomData<R>,
}

impl<R: CacheRule> FromRequestParts<Arc<ServerState>> for CacheScope<R> {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<ServerState>) -> WebResult<Self> {
        let Incoming {
            user,
            api_key,
            params,
            db,
        } = Incoming::<R::User>::read(parts, state).await?;
        let cache = load_cache(
            &db,
            user.caller(),
            api_key.as_ref(),
            params.param("cache")?,
            R::ACCESS,
        )
        .await?;
        Ok(Self {
            cache,
            user,
            api_key,
            db,
            params,
            rule: PhantomData,
        })
    }
}

pub struct TeamScope<R: TeamRule> {
    pub team: MTeam,
    pub role: Option<TeamRole>,
    pub user: MUser,
    pub api_key: Option<ApiKeyContext>,
    pub db: DbContext,
    pub params: PathParams,
    rule: PhantomData<R>,
}

impl<R: TeamRule> FromRequestParts<Arc<ServerState>> for TeamScope<R> {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<ServerState>) -> WebResult<Self> {
        let Incoming {
            user,
            api_key,
            params,
            db,
        } = Incoming::<MUser>::read(parts, state).await?;
        let (team, role) = load_team(
            &db.web_db,
            &user,
            api_key.as_ref(),
            params.param("team")?,
            R::ACCESS,
        )
        .await?;
        Ok(Self {
            team,
            role,
            user,
            api_key,
            db,
            params,
            rule: PhantomData,
        })
    }
}
