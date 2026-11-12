/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::{Permission, PermissionMask, mask_grants};
use gradient_db::exec_jobs::tokens::ResolvedJobToken;
use gradient_types::ids::CacheId;
use gradient_types::{
    ApiId, ExecJobAttemptId, ExecJobId, MSession, MUser, ProjectId, TaskId, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyContext {
    pub mask: PermissionMask,
    pub cache_permission_mask: PermissionMask,
    pub project: Option<ProjectId>,
    pub cache_pin: Option<CacheId>,
    pub allowed_ips: Vec<String>,
}

impl ApiKeyContext {
    pub fn has_full_account_access(&self) -> bool {
        self.project.is_none()
            && self.cache_pin.is_none()
            && mask_grants(self.mask, Permission::UseSuperuserRights)
    }

    pub fn may_create_projects(&self) -> bool {
        self.project.is_none()
            && self.cache_pin.is_none()
            && mask_grants(self.mask, Permission::CreateProject)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaybeApiKey(pub Option<ApiKeyContext>);

impl From<&DecodedRequest> for MaybeApiKey {
    fn from(decoded: &DecodedRequest) -> Self {
        decoded
            .api_key_context()
            .cloned()
            .map_or_else(MaybeApiKey::none, MaybeApiKey::from)
    }
}

impl From<ApiKeyContext> for MaybeApiKey {
    fn from(ctx: ApiKeyContext) -> Self {
        Self(Some(ctx))
    }
}

impl MaybeApiKey {
    pub fn none() -> Self {
        Self(None)
    }
    pub fn as_ref(&self) -> Option<&ApiKeyContext> {
        self.0.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobPrincipal {
    pub job: ExecJobId,
    pub attempt: ExecJobAttemptId,
    pub project: ProjectId,
    pub task: TaskId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Principal {
    User(UserId),
    Job(JobPrincipal),
}

impl Principal {
    pub fn job(&self) -> Option<&JobPrincipal> {
        match self {
            Principal::Job(job) => Some(job),
            Principal::User(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitBucketOwner {
    User(UserId),
    ApiKey(ApiId),
    Job(ExecJobId),
}

#[derive(Debug, Clone)]
pub enum DecodedRequest {
    Session {
        session: MSession,
    },
    ApiKey {
        user_id: UserId,
        key: ApiId,
        context: ApiKeyContext,
    },
    Job {
        user_id: UserId,
        context: ApiKeyContext,
        job: ResolvedJobToken,
    },
}

impl DecodedRequest {
    pub fn user_id(&self) -> UserId {
        match self {
            DecodedRequest::Session { session } => session.user_id,
            DecodedRequest::ApiKey { user_id, .. } | DecodedRequest::Job { user_id, .. } => {
                *user_id
            }
        }
    }

    pub fn api_key_context(&self) -> Option<&ApiKeyContext> {
        match self {
            DecodedRequest::Session { .. } => None,
            DecodedRequest::ApiKey { context, .. } | DecodedRequest::Job { context, .. } => {
                Some(context)
            }
        }
    }

    pub fn job(&self) -> Option<&ResolvedJobToken> {
        match self {
            DecodedRequest::Job { job, .. } => Some(job),
            DecodedRequest::Session { .. } | DecodedRequest::ApiKey { .. } => None,
        }
    }

    pub fn user_with_superuser_allowed_for_credential(&self, mut user: MUser) -> MUser {
        user.superuser &= match self {
            DecodedRequest::Session { .. } => true,
            DecodedRequest::ApiKey { context, .. } => context.has_full_account_access(),
            DecodedRequest::Job { .. } => false,
        };
        user
    }

    pub fn rate_limit_bucket(&self) -> RateLimitBucketOwner {
        match self {
            DecodedRequest::Session { session } => RateLimitBucketOwner::User(session.user_id),
            DecodedRequest::ApiKey { key, .. } => RateLimitBucketOwner::ApiKey(*key),
            DecodedRequest::Job { job, .. } => RateLimitBucketOwner::Job(job.job),
        }
    }

    pub fn principal(&self) -> Principal {
        match self.job() {
            Some(job) => Principal::Job(JobPrincipal {
                job: job.job,
                attempt: job.attempt,
                project: job.project,
                task: job.task,
            }),
            None => Principal::User(self.user_id()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{admin_mask, mask_from};

    fn superuser() -> MUser {
        MUser {
            superuser: true,
            ..Default::default()
        }
    }

    fn full_key() -> ApiKeyContext {
        ApiKeyContext {
            mask: mask_from(Permission::ALL),
            cache_permission_mask: 0,
            project: None,
            cache_pin: None,
            allowed_ips: Vec::new(),
        }
    }

    #[test]
    fn a_job_token_never_acts_as_superuser() {
        let job = DecodedRequest::Job {
            user_id: UserId::now_v7(),
            context: full_key(),
            job: ResolvedJobToken {
                job: ExecJobId::now_v7(),
                attempt: ExecJobAttemptId::now_v7(),
                project: ProjectId::now_v7(),
                task: TaskId::now_v7(),
                project_name: "infra".into(),
                task_name: "master".into(),
                acting_user: UserId::now_v7(),
            },
        };

        assert!(
            !job.user_with_superuser_allowed_for_credential(superuser())
                .superuser
        );
    }

    fn keeps_superuser(key: ApiKeyContext) -> bool {
        DecodedRequest::ApiKey {
            key: ApiId::now_v7(),
            user_id: UserId::now_v7(),
            context: key,
        }
        .user_with_superuser_allowed_for_credential(superuser())
        .superuser
    }

    #[test]
    fn an_api_key_with_use_superuser_rights_keeps_owner_superuser() {
        assert!(keeps_superuser(ApiKeyContext {
            mask: Permission::UseSuperuserRights.bit() | Permission::ViewProject.bit(),
            ..full_key()
        }));
    }

    #[test]
    fn api_key_with_all_project_permissions_without_superuser_rights_loses_owner_superuser_flag() {
        assert!(!keeps_superuser(ApiKeyContext {
            mask: admin_mask() | Permission::CreateProject.bit(),
            ..full_key()
        }));
    }

    #[test]
    fn pinned_api_key_loses_superuser_even_with_use_superuser_rights() {
        assert!(!keeps_superuser(ApiKeyContext {
            cache_pin: Some(CacheId::now_v7()),
            ..full_key()
        }));
        assert!(!keeps_superuser(ApiKeyContext {
            project: Some(ProjectId::now_v7()),
            ..full_key()
        }));
    }
}
