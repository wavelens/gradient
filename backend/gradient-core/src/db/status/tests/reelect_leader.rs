/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::{bid, did, make_ctx, org, run};
use super::super::leader_election::reelect_leader;
use crate::types::{BuildId, DerivationId, EvaluationId, OrganizationId};
use gradient_entity::build::{BuildStatus, Model as MBuild};
use gradient_entity::derivation::Model as MDerivation;
use sea_orm::{DatabaseBackend, MockDatabase};

fn build(id: BuildId, drv: DerivationId, via: Option<BuildId>, status: BuildStatus) -> MBuild {
    MBuild {
        id,
        evaluation: EvaluationId::now_v7(),
        derivation: drv,
        status,
        log_id: None,
        build_time_ms: None,
        worker: None,
        via,
        external_cached: false,
        attempt: 0,
        timeout_secs: None,
        max_silent_secs: None,
        prefer_local_build: false,
        created_at: chrono::NaiveDateTime::default(),
        updated_at: chrono::NaiveDateTime::default(),
        queued_at: None,
        ready_at: None,
        dispatched_at: None,
        build_started_at: None,
        build_finished_at: None,
    }
}

fn drv_row(id: DerivationId, owner: OrganizationId) -> MDerivation {
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

/// Same-org follower is promoted to leader; cross-org follower's via is cleared.
#[test]
fn promotes_same_org_and_orphans_cross_org_follower() {
    run(async {
        let leader_drv = did(1);
        let same_org_drv = did(2);
        let cross_org_drv = did(3);
        let leader = build(bid(10), leader_drv, None, BuildStatus::Queued);
        let same_org_follower = build(bid(11), same_org_drv, Some(leader.id), BuildStatus::Created);
        let cross_org_follower =
            build(bid(12), cross_org_drv, Some(leader.id), BuildStatus::Created);

        // Query sequence:
        //   1. EDerivation::find_by_id(leader.derivation) → drv with org(1)
        //   2. EBuild::find().filter(Via = leader.id) → [same_org_follower, cross_org_follower]
        //   3. EDerivation::find().filter(Id IN follower_drv_ids) → org map
        //   4. active.update() for promotion (Postgres RETURNING → query_results)
        //   5. EBuild::update_many (clear via for cross_org_follower → exec_results)
        // No "remaining same-org" update_many: only one same-org follower → skip(1) is empty.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv_row(leader_drv, org(1))]])
            .append_query_results([vec![same_org_follower.clone(), cross_org_follower.clone()]])
            .append_query_results([vec![
                drv_row(same_org_drv, org(1)),
                drv_row(cross_org_drv, org(2)),
            ]])
            .append_query_results([vec![same_org_follower.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let ctx = make_ctx(db);
        reelect_leader(&ctx, &leader)
            .await
            .expect("reelect ok");
    });
}

/// Leader has only cross-org followers: every follower's via is cleared.
#[test]
fn all_cross_org_followers_orphaned_when_no_same_org() {
    run(async {
        let leader_drv = did(1);
        let foll_drv_b = did(2);
        let foll_drv_c = did(3);
        let leader = build(bid(20), leader_drv, None, BuildStatus::Queued);
        let f1 = build(bid(21), foll_drv_b, Some(leader.id), BuildStatus::Created);
        let f2 = build(bid(22), foll_drv_c, Some(leader.id), BuildStatus::Created);

        // Query sequence:
        //   1. EDerivation::find_by_id(leader.derivation) → drv with org(1)
        //   2. EBuild::find().filter(Via = leader.id) → [f1, f2]
        //   3. EDerivation::find().filter(Id IN follower_drv_ids) → org map (all cross-org)
        //   4. EBuild::update_many (clear via for all cross-org → exec_results)
        // No same-org candidates, so no active.update() promotion.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv_row(leader_drv, org(1))]])
            .append_query_results([vec![f1.clone(), f2.clone()]])
            .append_query_results([vec![
                drv_row(foll_drv_b, org(2)),
                drv_row(foll_drv_c, org(3)),
            ]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();

        let ctx = make_ctx(db);
        reelect_leader(&ctx, &leader)
            .await
            .expect("reelect ok");
    });
}
