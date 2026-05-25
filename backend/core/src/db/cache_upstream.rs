/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use entity::cache_upstream::{Column as CCacheUpstream, Entity as ECacheUpstream};
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
                .add(CCacheUpstream::Url.is_not_null())
                .add(CCacheUpstream::Mode.ne(CacheSubscriptionMode::WriteOnly)),
        )
        .all(db)
        .await?;

    Ok(upstream_rows.into_iter().filter_map(|r| r.url).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity::cache_upstream;
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

    fn upstream_row(cache: CacheId, url: Option<&str>) -> cache_upstream::Model {
        cache_upstream::Model {
            id: crate::types::ids::CacheUpstreamId::now_v7(),
            cache,
            display_name: "test".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            upstream_cache: None,
            url: url.map(str::to_owned),
            public_key: None,
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
                upstream_row(cache_a, Some("https://cache-a.example/")),
                upstream_row(cache_b, Some("https://cache-b.example/")),
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
}
