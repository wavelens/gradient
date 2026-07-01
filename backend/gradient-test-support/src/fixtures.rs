/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deterministic fixture builders. Stable UUIDs make assertions readable:
//! you can write `assert_eq!(body["name"], "test-org")` instead of chasing
//! a random `Uuid`.

use gradient_entity::ids::{
    CacheId, CacheUpstreamId, CacheUserId, CommitId, EvaluationId, OrganizationCacheId,
    OrganizationId, ProjectId, UserId,
};
use gradient_entity::organization_cache::CacheSubscriptionMode;
use gradient_entity::*;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use sea_orm::{ActiveModelTrait, DatabaseConnection, DbErr, IntoActiveModel};
use uuid::Uuid;

pub fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
}
pub fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap())
}
pub fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap())
}
pub fn commit_id() -> CommitId {
    CommitId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000013").unwrap())
}

pub fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

pub fn org() -> organization::Model {
    organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Organization".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

pub fn user() -> user::Model {
    user::Model {
        id: user_id(),
        username: "testuser".into(),
        name: "Test User".into(),
        email: "test@example.com".into(),
        password: Some(password_auth::generate_hash("TestPass123!")),
        last_login_at: test_date(),
        created_at: test_date(),
        email_verified: true,
        active: true,
        ..Default::default()
    }
}

pub fn superuser_user() -> user::Model {
    user::Model {
        superuser: true,
        ..user()
    }
}

pub fn org_with_id(id: OrganizationId, slug: &str) -> organization::Model {
    organization::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
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

pub fn org_cache_link(
    id: OrganizationCacheId,
    org: OrganizationId,
    cache: CacheId,
    mode: CacheSubscriptionMode,
) -> organization_cache::Model {
    organization_cache::Model {
        id,
        organization: org,
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

pub fn eval_at(id: EvaluationId, offset_secs: i64) -> evaluation::Model {
    let created_at = test_date() + chrono::Duration::seconds(offset_secs);
    evaluation::Model {
        id,
        project: Some(project_id()),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status: evaluation::EvaluationStatus::Completed,
        created_at,
        updated_at: created_at,
        ..Default::default()
    }
}

/// Insert an Admin `cache_user` row for `user_id` on `cache_id`. Call this after
/// inserting a cache fixture to satisfy the invariant that every cache has at
/// least one Admin member.
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
