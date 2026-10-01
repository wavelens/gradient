/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Hourly pruning of every table that grows with time rather than with the
//! graph, so each stays bounded.
//!
//! Raw metric samples follow `MetricsArgs::retention_raw_days`, minute and hour
//! rollups `MetricsArgs::retention_rollup_days` (day and week rollups are kept),
//! and the remaining histories `ServerArgs::retention_days`. A `0` day-count
//! disables its group. Every delete takes at most [`PRUNE_BATCH`] rows and
//! repeats until a batch comes back short, so a backlog never holds a long lock.
//! Live rows are never taken: an open worker connection is kept unless a newer
//! connection of the same worker superseded it, the newest finished admin task
//! of each kind stays as its schedule's anchor, and a cluster job goes only once
//! finished.

use std::time::Duration;

use chrono::NaiveDateTime;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::metric_rollup::RollupGranularity;
use gradient_util::supervision::ChildSpec;
use sea_orm::{ConnectionTrait, DbErr};
use tracing::{debug, warn};

use crate::DbContext;
use crate::sql::Query;

const RETENTION_INTERVAL_SECS: u64 = 3600;
const PRUNE_BATCH: i64 = 5000;

crate::sql! {
    PRUNE_PHASE_EVENT = "DELETE FROM phase_event WHERE id IN \
        (SELECT id FROM phase_event WHERE at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_WORKER_SAMPLE = "DELETE FROM worker_sample WHERE id IN \
        (SELECT id FROM worker_sample WHERE at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_CACHE_METRIC = "DELETE FROM cache_metric WHERE id IN \
        (SELECT id FROM cache_metric WHERE bucket_time < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_UPSTREAM_METRIC = "DELETE FROM upstream_metric WHERE id IN \
        (SELECT id FROM upstream_metric WHERE bucket_time < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_DISPATCHED_JOB = "DELETE FROM dispatched_job WHERE id IN \
        (SELECT id FROM dispatched_job WHERE created_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_PENDING_DELIVERY = "DELETE FROM pending_delivery WHERE id IN \
        (SELECT id FROM pending_delivery \
         WHERE (delivered_at IS NOT NULL OR failed_at IS NOT NULL) \
           AND COALESCE(delivered_at, failed_at) < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_WORKER_CONNECTION = "DELETE FROM worker_connection WHERE id IN \
        (SELECT c.id FROM worker_connection c \
         WHERE c.disconnected_at < $1 \
            OR (c.disconnected_at IS NULL AND c.connected_at < $1 \
                AND EXISTS (SELECT 1 FROM worker_connection n \
                            WHERE n.worker_id = c.worker_id AND n.connected_at > c.connected_at)) \
         LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_WEBHOOK_DELIVERY = "DELETE FROM webhook_delivery WHERE id IN \
        (SELECT id FROM webhook_delivery WHERE delivered_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_TASK_ACTION_DELIVERY = "DELETE FROM task_action_delivery WHERE id IN \
        (SELECT id FROM task_action_delivery WHERE delivered_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_SESSION = "DELETE FROM session WHERE id IN \
        (SELECT id FROM session WHERE expires_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_CLI_DEVICE_AUTHORIZATION = "DELETE FROM cli_device_authorization WHERE id IN \
        (SELECT id FROM cli_device_authorization WHERE expires_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_ADMIN_TASK = "DELETE FROM admin_task WHERE id IN \
        (SELECT a.id FROM admin_task a \
         WHERE a.finished_at < $1 \
           AND EXISTS (SELECT 1 FROM admin_task n \
                       WHERE n.kind = a.kind AND n.finished_at > a.finished_at) \
         LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_AUDIT_LOG = "DELETE FROM audit_log WHERE id IN \
        (SELECT id FROM audit_log WHERE created_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_DERIVATION_METRIC = "DELETE FROM derivation_metric WHERE id IN \
        (SELECT id FROM derivation_metric WHERE created_at < $1 LIMIT $2)",
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;
}

fn prune_cluster_job_sql() -> String {
    format!(
        "DELETE FROM cluster_job WHERE id IN \
         (SELECT c.id FROM cluster_job c \
          WHERE c.status IN ({completed}, {failed}, {aborted}) AND (c.updated_at < $1 \
          OR NOT EXISTS (SELECT 1 FROM cluster_member m WHERE m.cluster_job = c.id)) \
          LIMIT $2)",
        completed = i16::from(ClusterJobStatus::Completed),
        failed = i16::from(ClusterJobStatus::Failed),
        aborted = i16::from(ClusterJobStatus::Aborted),
    )
}

fn prune_metric_rollup_sql() -> String {
    format!(
        "DELETE FROM metric_rollup WHERE id IN \
         (SELECT id FROM metric_rollup \
          WHERE granularity IN ({minute}, {hour}) AND bucket_start < $1 LIMIT $2)",
        minute = i16::from(RollupGranularity::Minute),
        hour = i16::from(RollupGranularity::Hour),
    )
}

crate::sql_fn! {
    PRUNE_CLUSTER_JOB = prune_cluster_job_sql,
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;

    PRUNE_METRIC_ROLLUP = prune_metric_rollup_sql,
        params = [Now, Int(PRUNE_BATCH)],
        tier = Sweep;
}

static RAW_METRICS: &[&Query] = &[
    &PRUNE_PHASE_EVENT,
    &PRUNE_WORKER_SAMPLE,
    &PRUNE_CACHE_METRIC,
    &PRUNE_UPSTREAM_METRIC,
];

static HISTORIES: &[&Query] = &[
    &PRUNE_DISPATCHED_JOB,
    &PRUNE_PENDING_DELIVERY,
    &PRUNE_WORKER_CONNECTION,
    &PRUNE_WEBHOOK_DELIVERY,
    &PRUNE_TASK_ACTION_DELIVERY,
    &PRUNE_SESSION,
    &PRUNE_CLI_DEVICE_AUTHORIZATION,
    &PRUNE_ADMIN_TASK,
    &PRUNE_AUDIT_LOG,
    &PRUNE_DERIVATION_METRIC,
    &PRUNE_CLUSTER_JOB,
];

static ROLLUPS: &[&Query] = &[&PRUNE_METRIC_ROLLUP];

/// The hourly pruning pass as a supervised child.
pub fn child_spec(ctx: DbContext) -> ChildSpec {
    ChildSpec::periodic(
        "retention",
        Duration::from_secs(RETENTION_INTERVAL_SECS),
        Duration::from_secs(600),
        move || {
            let ctx = ctx.clone();
            async move {
                run_retention(&ctx, gradient_types::now()).await;
                Ok(())
            }
        },
    )
}

async fn run_retention(ctx: &DbContext, now: NaiveDateTime) {
    let metrics = &ctx.config.metrics_args;
    let groups = [
        (metrics.retention_raw_days, RAW_METRICS),
        (ctx.config.server.retention_days, HISTORIES),
        (metrics.retention_rollup_days, ROLLUPS),
    ];

    for (days, prunes) in groups {
        if days > 0 {
            prune_all(&ctx.worker_db, prunes, now - chrono::Duration::days(days)).await;
        }
    }

    debug!("retention pass complete");
}

async fn prune_all<C: ConnectionTrait>(db: &C, prunes: &[&Query], cutoff: NaiveDateTime) {
    for prune in prunes {
        match prune_in_batches(db, prune, cutoff).await {
            Ok(0) => {}
            Ok(rows) => debug!(query = prune.name, rows, "retention pruned"),
            Err(e) => warn!(error = %e, query = prune.name, "retention failed"),
        }
    }
}

async fn prune_in_batches<C: ConnectionTrait>(
    db: &C,
    prune: &Query,
    cutoff: NaiveDateTime,
) -> Result<u64, DbErr> {
    let mut pruned = 0;
    loop {
        let rows = db
            .execute_raw(prune.bind([cutoff.into(), PRUNE_BATCH.into()]))
            .await?
            .rows_affected();
        pruned += rows;
        if rows < PRUNE_BATCH as u64 {
            return Ok(pruned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement, Value};

    fn affected(rows: &[u64]) -> Vec<MockExecResult> {
        rows.iter()
            .map(|&rows_affected| MockExecResult {
                last_insert_id: 0,
                rows_affected,
            })
            .collect()
    }

    fn deleting<'a>(log: &'a [Statement], table: &str) -> Option<&'a Statement> {
        let prefix = format!("DELETE FROM {table} ");
        log.iter().find(|s| s.sql.starts_with(&prefix))
    }

    fn cutoff(stmt: &Statement) -> Value {
        stmt.values.as_ref().expect("bound statement").0[0].clone()
    }

    #[tokio::test]
    async fn each_history_is_pruned_past_its_own_setting() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(affected(&[0; 16]))
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;
        let now = gradient_types::now();
        let days = |n| Value::from(now - chrono::Duration::days(n));
        let history = days(ctx.config.server.retention_days);
        let raw = days(ctx.config.metrics_args.retention_raw_days);

        run_retention(&ctx, now).await;

        drop(ctx);
        let log = crate::pool::raw_statements(pool.into_transaction_log());
        for table in [
            "dispatched_job",
            "pending_delivery",
            "worker_connection",
            "webhook_delivery",
            "task_action_delivery",
            "session",
            "cli_device_authorization",
            "admin_task",
            "audit_log",
            "derivation_metric",
            "cluster_job",
        ] {
            let stmt = deleting(&log, table).unwrap_or_else(|| panic!("{table}: {log:?}"));
            assert_eq!(cutoff(stmt), history, "{table}");
        }
        for table in [
            "phase_event",
            "worker_sample",
            "cache_metric",
            "upstream_metric",
        ] {
            let stmt = deleting(&log, table).unwrap_or_else(|| panic!("{table}: {log:?}"));
            assert_eq!(cutoff(stmt), raw, "{table}");
        }
    }

    #[tokio::test]
    async fn a_cluster_job_is_pruned_once_finished_and_old_or_orphaned() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(affected(&[0]))
            .into_connection();

        prune_all(&db, &[&PRUNE_CLUSTER_JOB], gradient_types::now()).await;

        let log = crate::pool::raw_statements(db.into_transaction_log());
        let sql = &deleting(&log, "cluster_job").expect("one delete").sql;
        let finished = [
            ClusterJobStatus::Completed,
            ClusterJobStatus::Failed,
            ClusterJobStatus::Aborted,
        ]
        .map(|s| i16::from(s).to_string())
        .join(", ");
        assert!(
            sql.contains(&format!("c.status IN ({finished}) AND ")),
            "an active cluster job is never taken: {sql}"
        );
        assert!(
            sql.contains(
                "(c.updated_at < $1 \
                 OR NOT EXISTS (SELECT 1 FROM cluster_member m WHERE m.cluster_job = c.id))"
            ),
            "a finished cluster job goes past the cutoff, or right away once its members are gone: {sql}"
        );
    }

    #[tokio::test]
    async fn an_open_worker_connection_is_pruned_only_once_superseded() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(affected(&[0]))
            .into_connection();

        prune_all(&db, &[&PRUNE_WORKER_CONNECTION], gradient_types::now()).await;

        let log = crate::pool::raw_statements(db.into_transaction_log());
        let sql = &deleting(&log, "worker_connection").expect("one delete").sql;
        let open_arm = sql
            .split(" OR ")
            .find(|arm| arm.contains("disconnected_at IS NULL"))
            .expect("an arm for open connections");
        assert!(
            open_arm.contains("n.worker_id = c.worker_id AND n.connected_at > c.connected_at"),
            "an open connection is taken only behind a newer one of its worker: {sql}"
        );
    }

    #[tokio::test]
    async fn a_full_batch_is_followed_by_another_until_one_comes_back_short() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(affected(&[PRUNE_BATCH as u64, PRUNE_BATCH as u64, 7]))
            .into_connection();

        let pruned = prune_in_batches(&db, &PRUNE_SESSION, gradient_types::now())
            .await
            .expect("mocked deletes");

        assert_eq!(pruned, 2 * PRUNE_BATCH as u64 + 7);
        assert_eq!(db.into_transaction_log().len(), 3);
    }
}
