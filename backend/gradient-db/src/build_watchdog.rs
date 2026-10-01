/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Detection for shared builds stranded in `Building` behind a dispatch that will
//! never report. The orphan re-queue moves them back once, through the graph
//! writer; a transaction that rolls back, or a `Assigned` transition that lands
//! after its claim already gave up, leaves the shared build `Building` with nobody
//! building it and nothing that would ever move it again.

use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_types::DerivationBuildId;
use sea_orm::{ConnectionTrait, DbErr};

use crate::status_sql;

/// `Building` shared builds whose newest attempt's dispatch closed as `Abandoned`,
/// untouched for at least `grace_secs`. The grace keeps an orphan re-queue that
/// is merely queued behind a slow graph writer from being sent twice.
fn stranded_building_shared_builds_sql(grace_secs: i64) -> String {
    format!(
        "SELECT db.id AS shared_build \
         FROM derivation_build db \
         JOIN LATERAL ( \
           SELECT dj.outcome FROM build_attempt ba \
           JOIN dispatched_job dj ON dj.id = ba.dispatched_job \
           WHERE ba.derivation_build = db.id \
           ORDER BY ba.created_at DESC LIMIT 1 \
         ) latest ON TRUE \
         WHERE db.status = {building} \
           AND latest.outcome = {abandoned} \
           AND db.updated_at < (now() AT TIME ZONE 'UTC') - make_interval(secs => {grace_secs})",
        building = status_sql::build(BuildStatus::Building),
        abandoned = i16::from(DispatchedJobOutcome::Abandoned),
    )
}

crate::sql_fn! {
    STRANDED_BUILDING_SHARED_BUILDS = || stranded_building_shared_builds_sql(900),
        params = [],
        tier = Sweep;
}

pub async fn stranded_building_shared_builds<C: ConnectionTrait>(
    db: &C,
    grace_secs: i64,
) -> Result<Vec<DerivationBuildId>, DbErr> {
    let rows = db
        .query_all_raw(
            STRANDED_BUILDING_SHARED_BUILDS
                .bind_built(stranded_building_shared_builds_sql(grace_secs), []),
        )
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "shared_build").ok())
        .map(DerivationBuildId::new)
        .collect())
}
