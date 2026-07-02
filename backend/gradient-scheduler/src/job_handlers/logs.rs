/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Log streaming.

use anyhow::Result;
use tracing::{debug, warn};

use gradient_types::*;

use crate::Scheduler;
use crate::jobs::PendingJob;

impl Scheduler {
    // ── Log streaming ─────────────────────────────────────────────────────────

    pub async fn append_log(&self, job_id: &str, task_index: u32, data: Vec<u8>) -> Result<()> {
        let bytes_len = data.len();
        let text = String::from_utf8_lossy(&data);
        let text = text.as_ref();

        let build_id_str = {
            let tracker = self.job_tracker.read().await;
            match tracker.active_job(job_id) {
                Some(PendingJob::Build(j)) => j
                    .job
                    .builds
                    .get(task_index as usize)
                    .map(|t| t.build_id.clone()),
                Some(PendingJob::Eval(_)) => {
                    debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: job is an eval, not a build");
                    return Ok(());
                }
                None => {
                    // Common shortly after a build finishes: a few in-flight
                    // log lines from the worker arrive after the job has
                    // already been removed from the active tracker. Not an
                    // error - just lost output, log at debug.
                    debug!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: no active job (likely race with completion)");
                    return Ok(());
                }
            }
        };

        let derivation_build: DerivationBuildId = match build_id_str.and_then(|s| s.parse::<DerivationBuildId>().ok()) {
            Some(id) => id,
            None => {
                warn!(%job_id, task_index, bytes = bytes_len, "log chunk dropped: build_task index out of range or derivation_build unparseable");
                return Ok(());
            }
        };

        let Some(attempt_id) =
            gradient_db::latest_attempt_id(&self.state.worker_db, derivation_build)
                .await
                .unwrap_or(None)
        else {
            debug!(%derivation_build, bytes = bytes_len, "log chunk dropped: no open attempt for anchor");
            return Ok(());
        };

        debug!(%derivation_build, %attempt_id, bytes = bytes_len, "appending build log");
        self.state.log_storage.append(attempt_id, text).await
    }
}
