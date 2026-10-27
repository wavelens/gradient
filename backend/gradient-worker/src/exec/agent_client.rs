/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use gradient_vm::ReadyAgent;
use gradient_wire::guest::{AgentToWorker, GuestConnection, RunJob, WorkerToAgent};
use tokio::net::UnixStream;

pub struct AgentClient {
    conn: GuestConnection<UnixStream>,
    agent_version: String,
}

impl From<ReadyAgent> for AgentClient {
    fn from(ready: ReadyAgent) -> Self {
        Self {
            conn: ready.conn,
            agent_version: ready.agent_version,
        }
    }
}

impl AgentClient {
    pub async fn attach(&mut self, output_offset: u64) -> Result<()> {
        self.conn
            .send(&WorkerToAgent::Attach { output_offset })
            .await
    }

    pub async fn run(&mut self, job: RunJob) -> Result<()> {
        self.conn.send(&WorkerToAgent::RunJob(Box::new(job))).await
    }

    pub async fn kill(&mut self) -> Result<()> {
        self.conn.send(&WorkerToAgent::Kill).await
    }

    pub async fn next(&mut self) -> Result<Option<AgentToWorker>> {
        self.conn.recv::<AgentToWorker>().await
    }
}
