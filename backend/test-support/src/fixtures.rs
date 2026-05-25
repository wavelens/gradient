/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deterministic fixture builders. Stable UUIDs make assertions readable:
//! you can write `assert_eq!(body["name"], "test-org")` instead of chasing
//! a random `Uuid`.

use entity::ids::{
    CacheId, CacheUpstreamId, CacheUserId, CommitId, EvaluationId, OrganizationCacheId,
    OrganizationId, ProjectId, UserId,
};
use entity::organization_cache::CacheSubscriptionMode;
use entity::*;
use gradient_core::types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, DbErr};
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
        description: "".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        public: false,
        hide_build_requests: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        github_installation_id: None,
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
        email_verification_token: None,
        email_verification_token_expires: None,
        managed: false,
        superuser: false,
        oidc_issuer: None,
        oidc_subject: None,
    }
}

pub fn org_with_id(id: OrganizationId, slug: &str) -> organization::Model {
    organization::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        description: String::new(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        public: false,
        hide_build_requests: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        github_installation_id: None,
    }
}

pub fn cache_with_id(id: CacheId, slug: &str, owner: UserId) -> cache::Model {
    cache::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        public: false,
        created_by: owner,
        created_at: test_date(),
        managed: false,
    }
}

pub fn org_cache_link(
    id: OrganizationCacheId,
    org: OrganizationId,
    cache: CacheId,
    mode: CacheSubscriptionMode,
) -> organization_cache::Model {
    organization_cache::Model { id, organization: org, cache, mode }
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
        upstream_cache: Some(upstream),
        url: None,
        public_key: None,
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
        upstream_cache: None,
        url: Some(url.into()),
        public_key: Some(public_key.into()),
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
        previous: None,
        next: None,
        created_at,
        updated_at: created_at,
        flake_source: None,
        check_run_ids: None,
        waiting_reason: None,
        trigger: None,
        concurrent: false,
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
    cache_user::ActiveModel {
        id: Set(CacheUserId::now_v7()),
        cache: Set(cache_id),
        user: Set(user_id),
        role: Set(BASE_CACHE_ROLE_ADMIN_ID),
    }
    .insert(db)
    .await?;
    Ok(())
}
