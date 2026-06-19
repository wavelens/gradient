/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::find_active_leaders;
use super::fixtures::{bid, did, org, run};
use gradient_entity::build::{BuildStatus, Model as MBuild};
use gradient_entity::ids::{BuildId, DerivationId};
use sea_orm::{DatabaseBackend, MockDatabase};
use uuid::Uuid;

fn build(id: BuildId, drv: DerivationId, status: BuildStatus, via: Option<BuildId>) -> MBuild {
    MBuild {
        id,
        evaluation: gradient_entity::ids::EvaluationId::now_v7(),
        derivation: drv,
        status,
        via,
        substitutable: false,
        substituted: false,
        attempt: 0,
        timeout_secs: None,
        max_silent_secs: None,
        prefer_local_build: false,
        created_at: chrono::NaiveDateTime::default(),
        updated_at: chrono::NaiveDateTime::default(),
        queued_at: None,
        ready_at: None,
        dispatched_at: None,
    }
}

#[test]
fn chunks_large_id_lists_under_postgres_param_cap() {
    run(async {
        // Postgres rejects any statement binding more than 65535 params. A
        // large monorepo restart funnels tens of thousands of derivation ids
        // through `find_active_leaders`; the now-single global lookup chunks
        // them so the `is_in` never overflows.
        const PG_MAX_PARAMS: usize = 65_535;
        let drv_ids: Vec<DerivationId> = (1..=70_000u128)
            .map(|i| DerivationId::new(Uuid::from_u128(i)))
            .collect();

        let mut db = MockDatabase::new(DatabaseBackend::Postgres);
        for _ in 0..4 {
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
fn single_active_build_is_its_own_leader() {
    run(async {
        let drv = did(1);
        let b = bid(10);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![build(b, drv, BuildStatus::Queued, None)]])
            .into_connection();

        let got = find_active_leaders(&db, org(1), &[drv]).await.unwrap();
        assert_eq!(got.get(&drv), Some(&b));
    });
}

#[test]
fn via_none_build_preferred_over_follower() {
    run(async {
        let drv = did(1);
        let leader = bid(10);
        let follower = bid(11);
        // Follower seen first, then the real (via = None) leader for the same
        // derivation: the leader must win.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                build(follower, drv, BuildStatus::Created, Some(leader)),
                build(leader, drv, BuildStatus::Building, None),
            ]])
            .into_connection();

        let got = find_active_leaders(&db, org(1), &[drv]).await.unwrap();
        assert_eq!(got.get(&drv), Some(&leader));
    });
}

#[test]
fn lone_follower_resolves_to_its_via_target() {
    run(async {
        let drv = did(1);
        let target = bid(99);
        let follower = bid(11);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![build(follower, drv, BuildStatus::Created, Some(target))]])
            .into_connection();

        let got = find_active_leaders(&db, org(1), &[drv]).await.unwrap();
        assert_eq!(got.get(&drv), Some(&target));
    });
}

#[test]
fn derivation_without_active_build_is_omitted() {
    run(async {
        let drv = did(1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuild>::new()])
            .into_connection();

        let got = find_active_leaders(&db, org(1), &[drv]).await.unwrap();
        assert!(!got.contains_key(&drv));
    });
}
