/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::leader_election::reelect_leader;
use super::fixtures::{bid, did, make_ctx, run};
use gradient_entity::build::{BuildStatus, Model as MBuild};
use gradient_types::{BuildId, DerivationId, EvaluationId};
use sea_orm::{DatabaseBackend, MockDatabase};

fn build(
    id: BuildId,
    drv: DerivationId,
    via: Option<BuildId>,
    status: BuildStatus,
    offset_secs: i64,
) -> MBuild {
    let t = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::seconds(offset_secs);
    MBuild {
        id,
        evaluation: EvaluationId::now_v7(),
        derivation: drv,
        status,
        via,
        substitutable: false,
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

/// The most-advanced follower (Building > Queued > Created) is promoted to
/// leader; the remaining followers are re-pointed at it. Derivations are
/// global, so there is no org split.
#[test]
fn promotes_most_advanced_then_repoints_rest() {
    run(async {
        let drv = did(1);
        let leader = build(bid(10), drv, None, BuildStatus::Queued, 0);
        let f_created = build(bid(11), drv, Some(leader.id), BuildStatus::Created, 10);
        let f_queued = build(bid(12), drv, Some(leader.id), BuildStatus::Queued, 20);
        let f_building = build(bid(13), drv, Some(leader.id), BuildStatus::Building, 30);

        // Query sequence:
        //   1. EBuild::find().filter(Via = leader.id) -> [created, queued, building]
        //   2. active.update() promoting f_building (Postgres RETURNING)
        //   3. EBuild::update_many re-pointing the remaining two (exec result)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![f_created.clone(), f_queued.clone(), f_building.clone()]])
            .append_query_results([vec![f_building.clone()]])
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 2,
            }])
            .into_connection();

        let ctx = make_ctx(db);
        reelect_leader(&ctx, &leader).await.expect("reelect ok");
    });
}

/// A leader with no followers needs no promotion and issues no further queries.
#[test]
fn no_followers_is_a_noop() {
    run(async {
        let drv = did(1);
        let leader = build(bid(10), drv, None, BuildStatus::Queued, 0);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuild>::new()])
            .into_connection();

        let ctx = make_ctx(db);
        reelect_leader(&ctx, &leader).await.expect("reelect ok");
    });
}
