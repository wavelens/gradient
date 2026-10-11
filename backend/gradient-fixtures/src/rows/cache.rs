/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{
    CacheId, CacheUpstreamId, CacheUserId, ProjectCacheId, ProjectId, RoleId, UserId,
};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_role, cache_upstream, cache_user, project_cache};
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
};
use gradient_types::permissions::{
    PermissionMask, cache_admin_mask, cache_view_mask, cache_write_mask,
};
use sea_orm::{ActiveModelTrait, DatabaseConnection, DbErr, IntoActiveModel};

use crate::ids::{cache_id, user_id};
use crate::test_date;

pub fn cache() -> cache::Model {
    cache::Model {
        display_name: "Test Cache".into(),
        ..cache_with_id(cache_id(), "test-cache", user_id())
    }
}

pub fn cache_with_id(id: CacheId, slug: &str, owner: UserId) -> cache::Model {
    cache::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        active: true,
        priority: 30,
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: owner,
        created_at: test_date(),
        ..Default::default()
    }
}

pub fn cache_admin_role() -> cache_role::Model {
    builtin_cache_role(BASE_CACHE_ROLE_ADMIN_ID, "Admin", cache_admin_mask())
}

pub fn cache_write_role() -> cache_role::Model {
    builtin_cache_role(BASE_CACHE_ROLE_WRITE_ID, "Write", cache_write_mask())
}

pub fn cache_view_role() -> cache_role::Model {
    builtin_cache_role(BASE_CACHE_ROLE_VIEW_ID, "View", cache_view_mask())
}

fn builtin_cache_role(id: RoleId, name: &str, permission: PermissionMask) -> cache_role::Model {
    cache_role::Model {
        id,
        name: name.into(),
        permission,
        managed: true,
        ..Default::default()
    }
}

pub fn cache_member(role: RoleId) -> cache_user::Model {
    cache_member_in(cache_id(), role)
}

pub fn cache_member_in(cache: CacheId, role: RoleId) -> cache_user::Model {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache,
        user: user_id(),
        role,
    }
}

pub fn project_cache_link(
    id: ProjectCacheId,
    project: ProjectId,
    cache: CacheId,
    mode: CacheSubscriptionMode,
) -> project_cache::Model {
    project_cache::Model {
        id,
        project,
        cache,
        mode,
    }
}

pub fn internal_upstream(
    id: CacheUpstreamId,
    cache: CacheId,
    upstream: CacheId,
) -> cache_upstream::Model {
    cache_upstream::Model {
        id,
        cache,
        display_name: "internal".into(),
        mode: CacheSubscriptionMode::ReadOnly,
        kind: cache_upstream::CacheUpstreamKind::Internal,
        upstream_cache: Some(upstream),
        ..Default::default()
    }
}

pub fn external_upstream(
    id: CacheUpstreamId,
    cache: CacheId,
    url: &str,
    public_key: &str,
) -> cache_upstream::Model {
    cache_upstream::Model {
        id,
        cache,
        display_name: "external".into(),
        mode: CacheSubscriptionMode::ReadOnly,
        kind: cache_upstream::CacheUpstreamKind::Http,
        url: Some(url.into()),
        public_key: Some(public_key.into()),
        ..Default::default()
    }
}

pub async fn insert_cache_creator_admin(
    db: &DatabaseConnection,
    cache_id: CacheId,
    user_id: UserId,
) -> Result<(), DbErr> {
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache: cache_id,
        user: user_id,
        role: BASE_CACHE_ROLE_ADMIN_ID,
    }
    .into_active_model()
    .insert(db)
    .await?;
    Ok(())
}
