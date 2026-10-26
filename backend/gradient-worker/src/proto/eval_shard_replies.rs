/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use gradient_util::sync::Mutex;
use gradient_wire::messages::TRANSFER_TIMEOUT;
use gradient_wire::types::EvalShardOutcome;
use tokio::sync::oneshot;

#[derive(Clone, Debug, Default)]
pub struct EvalShardReplies(Arc<Mutex<HashMap<String, oneshot::Sender<EvalShardOutcome>>>>);

impl EvalShardReplies {
    pub fn register(&self, job_id: &str) -> oneshot::Receiver<EvalShardOutcome> {
        let (reply, replied) = oneshot::channel();
        self.0.lock().insert(job_id.to_owned(), reply);
        replied
    }

    pub fn deliver(&self, job_id: &str, outcome: EvalShardOutcome) -> bool {
        match self.0.lock().remove(job_id) {
            Some(reply) => reply.send(outcome).is_ok(),
            None => false,
        }
    }

    pub fn forget_job(&self, job_id: &str) {
        self.0.lock().remove(job_id);
    }

    pub async fn wait(
        &self,
        job_id: &str,
        replied: oneshot::Receiver<EvalShardOutcome>,
    ) -> Result<EvalShardOutcome> {
        match tokio::time::timeout(TRANSFER_TIMEOUT, replied).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(anyhow!(
                "eval shard reply waiter dropped (job_id={job_id}) - connection closed?"
            )),
            Err(_) => {
                self.forget_job(job_id);
                Err(anyhow!("eval shard reply for job_id={job_id} timed out"))
            }
        }
    }
}
