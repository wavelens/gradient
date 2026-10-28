/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::PermissionMask;
use gradient_db::exec_jobs::tokens::ResolvedJobToken;
use gradient_types::ids::CacheId;
use gradient_types::{ExecJobAttemptId, ExecJobId, ProjectId, TaskId, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyContext {
    pub mask: PermissionMask,
    pub project: Option<ProjectId>,
    pub cache_pin: Option<CacheId>,
    pub cache_permission_mask: Option<i64>,
    pub allowed_ips: Vec<String>,
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

#[derive(Debug, Clone)]
pub enum DecodedRequest {
    Session {
        user_id: UserId,
    },
    ApiKey {
        user_id: UserId,
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
            DecodedRequest::Session { user_id }
            | DecodedRequest::ApiKey { user_id, .. }
            | DecodedRequest::Job { user_id, .. } => *user_id,
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
