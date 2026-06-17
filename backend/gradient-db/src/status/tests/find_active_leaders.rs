/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::{bid, cid, did, org, run};
use super::super::find_active_leaders;
use gradient_entity::build::{BuildStatus, Model as MBuild};
use gradient_entity::cache_upstream::Model as MCacheUpstream;
use gradient_entity::derivation::Model as MDerivation;
use gradient_entity::ids::{BuildId, DerivationId, OrganizationCacheId, OrganizationId};
use gradient_entity::organization_cache::{CacheSubscriptionMode, Model as MOrganizationCache};
use sea_orm::{DatabaseBackend, MockDatabase};
use uuid::Uuid;

fn build(
    id: BuildId,
    drv: DerivationId,
    status: BuildStatus,
    substitutable: bool,
    offset_secs: i64,
) -> MBuild {
    let t = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::seconds(offset_secs);
    MBuild {
        id,
        evaluation: gradient_entity::ids::EvaluationId::now_v7(),
        derivation: drv,
        status,
        via: None,
        substitutable,
        substituted: false,
        attempt: 0,
        timeout_secs: None,
        max_silent_secs: None,
        prefer_local_build: false,
        created_at: t,
        updated_at: t,
        queued_at: None,
        ready_at: None,
        dispatched_at: None,
    }
}

fn drv_row(id: DerivationId, owner: OrganizationId, _path: &str) -> MDerivation {
    MDerivation {
        id,
        organization: owner,
        hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        name: "x".into(),
        architecture: "x86_64-linux".into(),
        created_at: chrono::NaiveDateTime::default(),
        ..Default::default()
    }
}

#[test]
fn chunks_large_id_lists_under_postgres_param_cap() {
    run(async {
        // Postgres rejects any statement binding more than 65535 params. A
        // large monorepo restart funnels tens of thousands of derivation ids
        // through `find_active_leaders`; without chunking the `is_in`
        // overflows and `/evaluate` 500s ("too many arguments for query").
        const PG_MAX_PARAMS: usize = 65_535;
        let drv_ids: Vec<DerivationId> = (1..=70_000u128)
            .map(|i| DerivationId::new(Uuid::from_u128(i)))
            .collect();

        // Same-org pass and cross-org derivation lookup both return nothing,
        // so `drv_hashes` is empty and the function returns early. Empty
        // result sets satisfy every chunked query regardless of model type.
        let mut db = MockDatabase::new(DatabaseBackend::Postgres);
        for _ in 0..64 {
            db = db.append_query_results([Vec::<MBuild>::new()]);
        }
        let db = db.into_connection();

        let got = find_active_leaders(&db, org(1), &drv_ids).await.unwrap();
        assert!(got.is_empty());

        for txn in db.into_transaction_log() {
            for stmt in txn.statements() {
                let n = stmt.values.as_ref().map(|v| v.0.len()).unwrap_or(0);
                assert!(
                    n <= PG_MAX_PARAMS,
                    "statement bound {n} params over the {PG_MAX_PARAMS} cap: {}",
                    stmt.sql
                );
            }
        }
    });
}

#[test]
fn cross_org_match_when_no_same_org_candidate() {
    run(async {
        let drv_b = did(2);
        let drv_a = did(1);
        let leader_build = bid(10);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuild>::new()])
            .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(2),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadOnly,
            }]])
            .append_query_results([Vec::<MCacheUpstream>::new()])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(1),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadWrite,
            }]])
            .append_query_results([vec![drv_row(drv_a, org(1), "/nix/store/x.drv")]])
            .append_query_results([vec![build(leader_build, drv_a, BuildStatus::Building, false, 0)]])
            .into_connection();

        let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
        assert_eq!(got.get(&drv_b), Some(&leader_build), "got: {:?}", got);
    });
}

#[test]
fn cross_org_tie_break_most_advanced_then_oldest() {
    run(async {
        let drv_b = did(2);
        let drv_a = did(1);
        let drv_c = did(3);
        let queued_old = bid(20);
        let building_new = bid(21);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuild>::new()])
            .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(2),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadOnly,
            }]])
            .append_query_results([Vec::<MCacheUpstream>::new()])
            .append_query_results([vec![
                MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(1),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadWrite,
                },
                MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(3),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadWrite,
                },
            ]])
            .append_query_results([vec![
                drv_row(drv_a, org(1), "/nix/store/x.drv"),
                drv_row(drv_c, org(3), "/nix/store/x.drv"),
            ]])
            .append_query_results([vec![
                build(queued_old, drv_a, BuildStatus::Queued, false, 0),
                build(building_new, drv_c, BuildStatus::Building, false, 60),
            ]])
            .into_connection();

        let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
        assert_eq!(got.get(&drv_b), Some(&building_new), "got: {:?}", got);
    });
}

#[test]
fn same_org_preferred_over_cross_org() {
    run(async {
        let drv_b = did(2);
        let same_org_build = bid(30);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![build(same_org_build, drv_b, BuildStatus::Queued, false, 0)]])
            .into_connection();

        let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
        assert_eq!(got.get(&drv_b), Some(&same_org_build));
    });
}

#[test]
fn cross_org_external_cached_candidate_skipped() {
    run(async {
        let drv_b = did(2);
        let drv_a = did(1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuild>::new()])
            .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(2),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadOnly,
            }]])
            .append_query_results([Vec::<MCacheUpstream>::new()])
            .append_query_results([vec![MOrganizationCache {
                id: OrganizationCacheId::now_v7(),
                organization: org(1),
                cache: cid(1),
                mode: CacheSubscriptionMode::ReadWrite,
            }]])
            .append_query_results([vec![drv_row(drv_a, org(1), "/nix/store/x.drv")]])
            .append_query_results([Vec::<MBuild>::new()])
            .into_connection();

        let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
        assert!(!got.contains_key(&drv_b), "external_cached must be skipped");
    });
}
