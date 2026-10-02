/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::PermissionMask;
use gradient_types::ids::CacheId;
use gradient_types::{ApiId, ProjectId, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyContext {
    pub api_id: ApiId,
    pub mask: PermissionMask,
    pub project: Option<ProjectId>,
    pub cache_pin: Option<CacheId>,
    pub cache_permission_mask: Option<i64>,
    pub allowed_ips: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaybeApiKey(pub Option<ApiKeyContext>);

impl MaybeApiKey {
    pub fn none() -> Self {
        Self(None)
    }
    pub fn from_key(ctx: ApiKeyContext) -> Self {
        Self(Some(ctx))
    }
    pub fn as_ref(&self) -> Option<&ApiKeyContext> {
        self.0.as_ref()
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
}

impl DecodedRequest {
    pub fn user_id(&self) -> UserId {
        match self {
            DecodedRequest::Session { user_id } => *user_id,
            DecodedRequest::ApiKey { user_id, .. } => *user_id,
        }
    }

    pub fn api_key_context(&self) -> Option<&ApiKeyContext> {
        match self {
            DecodedRequest::Session { .. } => None,
            DecodedRequest::ApiKey { context, .. } => Some(context),
        }
    }
}
