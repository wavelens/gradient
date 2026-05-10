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
use gradient_core::types::{ApiId, OrganizationId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiKeyContext {
    /// The id of the `api` row this request was authenticated against.
    pub api_id: ApiId,
    /// The key's permission bitmask. The access layer intersects this with
    /// the user's role-derived mask before granting any capability.
    pub mask: PermissionMask,
    /// `None` for unscoped keys; `Some(id)` pins the key to a single org —
    /// requests for any other org are short-circuited as not-found.
    pub organization: Option<OrganizationId>,
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
        };
        let wrapped = MaybeApiKey::from_key(ctx);
        assert_eq!(wrapped.as_ref(), Some(&ctx));
    }

    #[test]
    fn none_returns_none_ref() {
        assert!(MaybeApiKey::none().as_ref().is_none());
    }
}
