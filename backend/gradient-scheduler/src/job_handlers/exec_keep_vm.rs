/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_db::exec_jobs::{attempts, keep_vm_running};
use gradient_db::scheduling::assignment_record::exec_job_of_key;
use gradient_pool::session_port::SessionSignal;
use gradient_types::EExecJob;
use gradient_types::ids::ExecJobId;
use sea_orm::EntityTrait;
use tracing::warn;

use crate::Scheduler;
use crate::jobs::exec_job_key;

impl Scheduler {
    pub async fn keep_exec_vm_running(&self, job: ExecJobId, keep: bool) -> Result<bool> {
        let nodes = keep_vm_running::set(&self.db.worker_db, job, keep).await?;
        let mut signals = Vec::new();
        for node in &nodes {
            if let Some(open) = attempts::open_of_job(&self.db.worker_db, node.id).await? {
                signals.push((open.worker, keep_signal(node.id, keep)));
            }
        }
        self.signal_workers(signals).await;
        Ok(!nodes.is_empty())
    }

    pub(crate) async fn resend_keep_vm_running(&self, worker_id: &str, job_id: &str) {
        let Some(exec_job) = exec_job_of_key(job_id) else {
            return;
        };
        match EExecJob::find_by_id(exec_job).one(&self.db.worker_db).await {
            Ok(Some(row)) if row.keep_vm_running => {
                let signal = keep_signal(exec_job, true);
                self.signal_workers(vec![(worker_id.to_owned(), signal)])
                    .await;
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, %job_id, "reading the keep VM running switch failed"),
        }
    }
}

fn keep_signal(job: ExecJobId, keep_vm_running: bool) -> SessionSignal {
    SessionSignal::ExecKeepVmRunning {
        job_id: exec_job_key(job),
        keep_vm_running,
    }
}
