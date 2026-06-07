/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use entity::cache_upstream::{CacheUpstreamKind, Column as CCacheUpstream, Entity as ECacheUpstream};
use entity::organization_cache::{
    CacheSubscriptionMode, Column as COrganizationCache, Entity as EOrganizationCache,
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::types::ids::{CacheId, OrganizationId};

pub async fn upstream_urls_for_org<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<Vec<String>> {
    let org_cache_rows = EOrganizationCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(COrganizationCache::Organization.eq(org_id))
                .add(COrganizationCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    let cache_ids: Vec<CacheId> = org_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    let upstream_rows = ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Kind.eq(CacheUpstreamKind::Http))
                .add(CCacheUpstream::Url.is_not_null())
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    Ok(upstream_rows.into_iter().filter_map(|r| r.url).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradientProtoUpstream {
    pub url: String,
    pub remote_cache: String,
    pub public_key: Option<String>,
    pub api_key_enc: Option<String>,
}

pub async fn gradient_proto_upstreams_for_org<C: ConnectionTrait>(
    db: &C,
    org_id: OrganizationId,
) -> Result<Vec<GradientProtoUpstream>> {
    let org_cache_rows = EOrganizationCache::find()
        .filter(
            sea_orm::Condition::all()
                .add(COrganizationCache::Organization.eq(org_id))
                .add(COrganizationCache::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    let cache_ids: Vec<CacheId> = org_cache_rows.iter().map(|r| r.cache).collect();
    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    let upstream_rows = ECacheUpstream::find()
        .filter(
            sea_orm::Condition::all()
                .add(CCacheUpstream::Cache.is_in(cache_ids))
                .add(CCacheUpstream::Kind.eq(CacheUpstreamKind::GradientProto))
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    Ok(upstream_rows
        .into_iter()
        .filter_map(|r| {
            Some(GradientProtoUpstream {
                url: r.url?,
                remote_cache: r.remote_cache_name?,
                public_key: r.public_key,
                api_key_enc: r.api_key,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::cache_upstream::{self, CacheUpstreamKind};
    use entity::organization_cache::{self, CacheSubscriptionMode};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org_cache_row(
        org: OrganizationId,
        cache: CacheId,
        mode: CacheSubscriptionMode,
    ) -> organization_cache::Model {
        organization_cache::Model {
            id: crate::types::ids::OrganizationCacheId::now_v7(),
            organization: org,
            cache,
            mode,
        }
    }

    fn upstream_row(cache: CacheId, kind: CacheUpstreamKind, url: Option<&str>) -> cache_upstream::Model {
        cache_upstream::Model {
            id: crate::types::ids::CacheUpstreamId::now_v7(),
            cache,
            display_name: "test".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind,
            url: url.map(str::to_owned),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn returns_urls_from_subscribed_caches() {
        let org = OrganizationId::new(Uuid::now_v7());
        let cache_a = CacheId::new(Uuid::now_v7());
        let cache_b = CacheId::new(Uuid::now_v7());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                org_cache_row(org, cache_a, CacheSubscriptionMode::ReadOnly),
                org_cache_row(org, cache_b, CacheSubscriptionMode::ReadWrite),
            ]])
            .append_query_results([vec![
                upstream_row(cache_a, CacheUpstreamKind::Http, Some("https://cache-a.example/")),
                upstream_row(cache_b, CacheUpstreamKind::Http, Some("https://cache-b.example/")),
            ]])
            .into_connection();

        let urls = upstream_urls_for_org(&db, org)
            .await
            .expect("helper succeeds");
        assert_eq!(
            urls,
            vec![
                "https://cache-a.example/".to_string(),
                "https://cache-b.example/".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn empty_when_no_org_caches() {
        let org = OrganizationId::new(Uuid::now_v7());

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<organization_cache::Model>::new()])
            .into_connection();

        let urls = upstream_urls_for_org(&db, org)
            .await
            .expect("helper succeeds");
        assert!(urls.is_empty());
    }

    #[tokio::test]
    async fn http_urls_excludes_gradient_proto() {
        let org = OrganizationId::new(Uuid::now_v7());
        let cache_a = CacheId::new(Uuid::now_v7());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![org_cache_row(org, cache_a, CacheSubscriptionMode::ReadOnly)]])
            .append_query_results([vec![
                upstream_row(cache_a, CacheUpstreamKind::Http, Some("https://http.example/")),
            ]])
            .into_connection();
        let urls = upstream_urls_for_org(&db, org).await.expect("ok");
        assert_eq!(urls, vec!["https://http.example/".to_string()]);
    }
}
