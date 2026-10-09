/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod passes;

use crate::units::{Step, next_unit};
use anyhow::Result;
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::maintenance::admin_tasks::{self, InsertPendingError};
use gradient_entity::ids::AdminTaskId;
use gradient_types::events::gc::DeepFinished;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeepGcReport {
    pub nars_scanned: u64,
    pub orphan_nars_removed: u64,
    pub zombie_cached_paths_purged: u64,
    pub blobs_scanned: u64,
    pub orphan_blobs_removed: u64,
    pub zombie_blob_rows_purged: u64,
    pub blob_check_errors: u64,
    pub logs_scanned: u64,
    pub orphan_logs_removed: u64,
    pub stale_partials_removed: u64,
}

impl DeepGcReport {
    fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|e| {
            warn!(error = ?e, "deep_gc: report serialization failed");
            serde_json::Value::Null
        })
    }
}

struct Round {
    id: AdminTaskId,
    started_at: NaiveDateTime,
    checkpoint: Option<String>,
    report: DeepGcReport,
    requested: bool,
}

pub(super) async fn step(state: &Arc<ServerState>) -> Result<Step> {
    let Some(round) = current_round(state).await? else {
        return Ok(Step::Idle);
    };
    let units = passes::units();
    let mut report = round.report;

    match next_unit(&units, round.checkpoint.as_deref()) {
        Some(unit) => {
            passes::run(state, unit, &mut report).await?;
            let saved = admin_tasks::save_checkpoint(
                &state.worker_db,
                round.id,
                round.started_at,
                unit,
                report.to_json(),
            )
            .await?;
            if !saved {
                info!(task_id = %round.id, "deep_gc: round restarted during {unit}");
            }
        }
        None => finish(state, round.id, round.started_at, report).await?,
    }

    Ok(if round.requested {
        Step::Requested
    } else {
        Step::Paced
    })
}

async fn current_round(state: &Arc<ServerState>) -> Result<Option<Round>> {
    let db = &state.worker_db;
    let task = match admin_tasks::find_active(db, AdminTaskKind::DeepGc).await? {
        Some(task) => task,
        None if background_round_due(state).await? => {
            match admin_tasks::insert_pending(db, AdminTaskKind::DeepGc, None).await {
                Ok(task) => task,
                Err(InsertPendingError::AlreadyActive(_)) => return Ok(None),
                Err(InsertPendingError::Db(e)) => return Err(e),
            }
        }
        None => return Ok(None),
    };

    let started_at = match (task.status, task.started_at) {
        (AdminTaskStatus::Running, Some(at)) => at,
        _ => match admin_tasks::start(db, task.id).await? {
            Some(at) => at,
            None => return Ok(None),
        },
    };

    Ok(Some(Round {
        id: task.id,
        started_at,
        checkpoint: task.checkpoint,
        report: task
            .progress
            .and_then(|p| serde_json::from_value(p).ok())
            .unwrap_or_default(),
        requested: task.created_by.is_some(),
    }))
}

async fn background_round_due(state: &ServerState) -> Result<bool> {
    let interval = state.config.gc.deep_interval_secs;
    if interval == 0 {
        return Ok(false);
    }

    let last = admin_tasks::last_finished_at(&state.worker_db, AdminTaskKind::DeepGc).await?;
    Ok(last.is_none_or(|at| (now() - at).num_seconds() >= interval as i64))
}

async fn finish(
    state: &Arc<ServerState>,
    id: AdminTaskId,
    started_at: NaiveDateTime,
    report: DeepGcReport,
) -> Result<()> {
    if !admin_tasks::complete(&state.worker_db, id, started_at, report.to_json()).await? {
        return Ok(());
    }

    info!(?report, task_id = %id, "deep_gc completed");
    state
        .record(DeepFinished {
            report: report.to_json(),
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_entity::ids::UserId;
    use gradient_storage::NarStore;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn state(db: MockDatabase, deep_interval_secs: u64) -> Arc<ServerState> {
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();
        test_server_state(nar, db.into_connection(), |config| {
            config.gc.deep_interval_secs = deep_interval_secs;
        })
    }

    #[tokio::test]
    async fn no_background_round_starts_when_the_interval_is_zero() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MAdminTask>::new()]);

        assert_eq!(step(&state(db, 0)).await.unwrap(), Step::Idle);
    }

    #[tokio::test]
    async fn a_requested_round_completes_after_its_last_unit_without_pacing() {
        let running = MAdminTask {
            id: AdminTaskId::now_v7(),
            kind: AdminTaskKind::DeepGc,
            status: AdminTaskStatus::Running,
            started_at: Some(now()),
            checkpoint: passes::units().last().cloned(),
            created_by: Some(UserId::now_v7()),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![running]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }]);

        assert_eq!(step(&state(db, 3600)).await.unwrap(), Step::Requested);
    }
}
