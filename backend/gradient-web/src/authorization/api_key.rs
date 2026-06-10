/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-request API-key context.
//!
//! When the auth middleware authenticates a request via `GRAD<key>` it inserts
//! `Extension(MaybeApiKey(Some(ctx)))` into the request; session-JWT requests
//! get `MaybeApiKey(None)`. The access layer reads this extension to
//! intersect the key's permission mask with the user's role-derived mask, and
//! to short-circuit on a pinned-org mismatch.

use crate::permissions::PermissionMask;
use gradient_types::ids::CacheId;
use gradient_types::{ApiId, OrganizationId, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyContext {
    pub api_id: ApiId,
    pub mask: PermissionMask,
    /// `None` = unscoped; `Some(id)` pins the key to a single org.
    pub organization: Option<OrganizationId>,
    /// `None` = unscoped; `Some(id)` pins the key to a single cache.
    pub cache_pin: Option<CacheId>,
    /// Cache-permission mask. `None` means unrestricted (i64::MAX).
    pub cache_permission_mask: Option<i64>,
    /// Source-IP allowlist (CIDR strings). Empty = any source allowed.
    pub allowed_ips: Vec<String>,
}

/// Extension type inserted on every authenticated request.
/// `Some(ctx)` for API-key requests, `None` for session-JWT requests.
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

/// Result of decoding a request token: a session JWT or an API key.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Permission, mask_from};
    use uuid::uuid;

    #[test]
    fn from_key_preserves_fields() {
        let ctx = ApiKeyContext {
            api_id: ApiId::new(uuid!("a0000000-0000-0000-0000-000000000010")),
            mask: mask_from(&[Permission::ViewOrg, Permission::TriggerEvaluation]),
            organization: Some(OrganizationId::new(uuid!(
                "a0000000-0000-0000-0000-000000000020"
            ))),
            cache_pin: None,
            cache_permission_mask: None,
            allowed_ips: Vec::new(),
        };
        let wrapped = MaybeApiKey::from_key(ctx.clone());
        assert_eq!(wrapped.as_ref(), Some(&ctx));
    }

    #[test]
    fn none_returns_none_ref() {
        assert!(MaybeApiKey::none().as_ref().is_none());
    }

    #[test]
    fn decoded_request_session_carries_no_api_key() {
        let outcome = DecodedRequest::Session {
            user_id: UserId::new(uuid!("a0000000-0000-0000-0000-000000000004")),
        };
        assert!(outcome.api_key_context().is_none());
        assert_eq!(
            outcome.user_id(),
            UserId::new(uuid!("a0000000-0000-0000-0000-000000000004"))
        );
    }

    #[test]
    fn decoded_request_api_key_carries_context() {
        let api_id = ApiId::new(uuid!("a0000000-0000-0000-0000-000000000011"));
        let user_id = UserId::new(uuid!("a0000000-0000-0000-0000-000000000004"));
        let outcome = DecodedRequest::ApiKey {
            user_id,
            context: ApiKeyContext {
                api_id,
                mask: mask_from(&[Permission::ViewOrg, Permission::TriggerEvaluation]),
                organization: None,
                cache_pin: None,
                cache_permission_mask: None,
                allowed_ips: Vec::new(),
            },
        };
        let ctx = outcome.api_key_context().expect("present");
        assert_eq!(ctx.api_id, api_id);
        assert_eq!(
            ctx.mask,
            mask_from(&[Permission::ViewOrg, Permission::TriggerEvaluation])
        );
        assert!(ctx.organization.is_none());
        assert_eq!(outcome.user_id(), user_id);
    }
}
