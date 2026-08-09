/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Compute the set of projects whose Gradient build outputs a given
//! project can substitute through its cache subscriptions and the
//! `cache_upstream` graph.
//!
//! Two projects are "cache-connected" when the writer project pushes into
//! a cache that lies in the upstream closure of one of the reader project's
//! caches. External (URL-based) upstreams are excluded - they don't host
//! Gradient builds.

use std::collections::{HashSet, VecDeque};

use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

use gradient_entity::cache_upstream::{Column as CCacheUpstream, Entity as ECacheUpstream};
use gradient_entity::ids::{CacheId, ProjectId};
use gradient_entity::project_cache::{
    CacheSubscriptionMode, Column as CProjectCache, Entity as EProjectCache,
};

/// Returns every project (including `reader_project` itself) whose build
/// outputs `reader_project` could substitute through its current cache
/// subscriptions and the `cache_upstream` graph.
///
/// Algorithm:
/// 1. Load reader's `project_cache` rows with mode `ReadWrite`/`ReadOnly`.
/// 2. BFS forward over `cache_upstream` edges (`cache → upstream_cache`) to
///    compute the upstream closure of the reader's caches. Cycles tolerated.
/// 3. Load every `project_cache` row with mode `ReadWrite`/`WriteOnly`
///    on any cache in that closure; return the distinct project ids.
pub async fn writer_projects_reachable_from<C: ConnectionTrait>(
    db: &C,
    reader_project: ProjectId,
) -> Result<HashSet<ProjectId>, DbErr> {
    let reader_rows = EProjectCache::find()
        .filter(CProjectCache::Project.eq(reader_project))
        .filter(CProjectCache::Mode.is_in(vec![
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::ReadOnly,
        ]))
        .all(db)
        .await?;
    let seed: Vec<CacheId> = reader_rows.into_iter().map(|r| r.cache).collect();

    let upstream_rows = ECacheUpstream::find()
        .filter(CCacheUpstream::UpstreamCache.is_not_null())
        .all(db)
        .await?;

    let mut closure: HashSet<CacheId> = seed.iter().copied().collect();
    let mut queue: VecDeque<CacheId> = seed.into_iter().collect();
    while let Some(cache_id) = queue.pop_front() {
        for edge in &upstream_rows {
            if edge.cache != cache_id {
                continue;
            }
            let Some(up) = edge.upstream_cache else {
                continue;
            };
            if closure.insert(up) {
                queue.push_back(up);
            }
        }
    }

    if closure.is_empty() {
        return Ok(HashSet::new());
    }

    let writer_rows = EProjectCache::find()
        .filter(CProjectCache::Cache.is_in(closure.into_iter().collect::<Vec<_>>()))
        .filter(CProjectCache::Mode.is_in(vec![
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::WriteOnly,
        ]))
        .all(db)
        .await?;
    Ok(writer_rows.into_iter().map(|r| r.project).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::cache_upstream::{CacheUpstreamKind, Model as MCacheUpstream};
    use gradient_entity::ids::{CacheId, CacheUpstreamId, ProjectCacheId, ProjectId};
    use gradient_entity::project_cache::Model as MProjectCache;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn project(n: u8) -> ProjectId {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        ProjectId::new(Uuid::from_bytes(bytes))
    }

    fn cid(n: u8) -> CacheId {
        let mut bytes = [0u8; 16];
        bytes[14] = n;
        CacheId::new(Uuid::from_bytes(bytes))
    }

    fn project_cache(
        project_id: ProjectId,
        cache_id: CacheId,
        mode: CacheSubscriptionMode,
    ) -> MProjectCache {
        MProjectCache {
            id: ProjectCacheId::now_v7(),
            project: project_id,
            cache: cache_id,
            mode,
        }
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn direct_overlap_reader_sees_writer() {
        run(async {
            // Reader (project B) reads cache X; writer (project A) writes cache X.
            let cache_x = cid(1);
            let reader_rows = vec![project_cache(
                project(2),
                cache_x,
                CacheSubscriptionMode::ReadOnly,
            )];
            let upstream_rows: Vec<MCacheUpstream> = vec![];
            let writer_rows = vec![
                project_cache(project(1), cache_x, CacheSubscriptionMode::ReadWrite),
                project_cache(project(2), cache_x, CacheSubscriptionMode::ReadOnly),
            ];

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([reader_rows])
                .append_query_results([upstream_rows])
                .append_query_results([writer_rows])
                .into_connection();

            let got = writer_projects_reachable_from(&db, project(2))
                .await
                .expect("query succeeds");

            assert!(got.contains(&project(1)), "got: {:?}", got);
        });
    }

    #[test]
    fn transitive_internal_chain() {
        run(async {
            // chain: cache_a → upstream cache_b → upstream cache_c
            // reader on a, writer on c
            let a = cid(1);
            let b = cid(2);
            let c = cid(3);

            let reader_rows = vec![project_cache(
                project(2),
                a,
                CacheSubscriptionMode::ReadOnly,
            )];
            let upstream_rows = vec![
                MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: a,
                    display_name: "ab".into(),
                    mode: CacheSubscriptionMode::ReadOnly,
                    kind: CacheUpstreamKind::Internal,
                    upstream_cache: Some(b),
                    url: None,
                    public_key: None,
                    remote_cache_name: None,
                    api_key: None,
                },
                MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: b,
                    display_name: "bc".into(),
                    mode: CacheSubscriptionMode::ReadOnly,
                    kind: CacheUpstreamKind::Internal,
                    upstream_cache: Some(c),
                    url: None,
                    public_key: None,
                    remote_cache_name: None,
                    api_key: None,
                },
            ];
            let writer_rows = vec![project_cache(
                project(1),
                c,
                CacheSubscriptionMode::ReadWrite,
            )];

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([reader_rows])
                .append_query_results([upstream_rows])
                .append_query_results([writer_rows])
                .into_connection();

            let got = writer_projects_reachable_from(&db, project(2))
                .await
                .unwrap();
            assert!(got.contains(&project(1)), "got: {:?}", got);
        });
    }

    #[test]
    fn external_upstream_skipped() {
        run(async {
            // Reader on a; the production helper filters `upstream_cache IS NOT NULL`,
            // so an external (URL-based) upstream row never reaches the BFS. Writer on
            // a separate cache b is therefore unreachable.
            let a = cid(1);
            let b = cid(2);

            let reader_rows = vec![project_cache(
                project(2),
                a,
                CacheSubscriptionMode::ReadOnly,
            )];
            let upstream_rows: Vec<MCacheUpstream> = vec![];
            let writer_rows: Vec<MProjectCache> = vec![];

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([reader_rows])
                .append_query_results([upstream_rows])
                .append_query_results([writer_rows])
                .into_connection();

            let got = writer_projects_reachable_from(&db, project(2))
                .await
                .unwrap();
            assert!(
                !got.contains(&project(1)),
                "external upstream must not reach project 1, got: {:?}",
                got
            );
            let _ = b;
        });
    }

    #[test]
    fn write_only_reader_excluded() {
        run(async {
            // Reader has WriteOnly on cache X; the production helper's mode filter
            // (`ReadWrite`/`ReadOnly`) returns no reader rows, so the closure is
            // empty and no writers are discovered.
            let x = cid(1);
            let reader_rows: Vec<MProjectCache> = vec![];
            let upstream_rows: Vec<MCacheUpstream> = vec![];
            let writer_rows: Vec<MProjectCache> = vec![];

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([reader_rows])
                .append_query_results([upstream_rows])
                .append_query_results([writer_rows])
                .into_connection();

            let got = writer_projects_reachable_from(&db, project(2))
                .await
                .unwrap();
            assert!(
                got.is_empty(),
                "WriteOnly reader must see nobody, got: {:?}",
                got
            );
            let _ = x;
        });
    }

    #[test]
    fn cycle_tolerated() {
        run(async {
            // cache_a.upstream = b; cache_b.upstream = a → cycle. BFS must
            // terminate via the visited set.
            let a = cid(1);
            let b = cid(2);

            let reader_rows = vec![project_cache(
                project(2),
                a,
                CacheSubscriptionMode::ReadOnly,
            )];
            let upstream_rows = vec![
                MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: a,
                    display_name: "ab".into(),
                    mode: CacheSubscriptionMode::ReadOnly,
                    kind: CacheUpstreamKind::Internal,
                    upstream_cache: Some(b),
                    url: None,
                    public_key: None,
                    remote_cache_name: None,
                    api_key: None,
                },
                MCacheUpstream {
                    id: CacheUpstreamId::now_v7(),
                    cache: b,
                    display_name: "ba".into(),
                    mode: CacheSubscriptionMode::ReadOnly,
                    kind: CacheUpstreamKind::Internal,
                    upstream_cache: Some(a),
                    url: None,
                    public_key: None,
                    remote_cache_name: None,
                    api_key: None,
                },
            ];
            let writer_rows = vec![
                project_cache(project(1), b, CacheSubscriptionMode::ReadWrite),
                project_cache(project(2), a, CacheSubscriptionMode::ReadOnly),
            ];

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([reader_rows])
                .append_query_results([upstream_rows])
                .append_query_results([writer_rows])
                .into_connection();

            let got = writer_projects_reachable_from(&db, project(2))
                .await
                .unwrap();
            assert!(
                got.contains(&project(1)),
                "cycle must still include reachable writer, got: {:?}",
                got
            );
        });
    }
}
