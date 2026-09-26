/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::firehose;
use crate::ids::{DerivationBuildId, EvaluationId, ProjectId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connected {
    pub worker_id: String,
    pub projects: Vec<ProjectId>,
}

firehose!(Connected, "worker.connected");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Disconnected {
    pub worker_id: String,
}

firehose!(Disconnected, "worker.disconnected");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobDispatched {
    pub project: ProjectId,
    pub worker_id: String,
    pub kind: i16,
    pub score: f64,
    pub build_id: Option<DerivationBuildId>,
    pub evaluation_id: EvaluationId,
}
firehose!(JobDispatched, "worker.job_dispatched");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueDepth {
    pub workers: usize,
    pub pending: usize,
    pub active: usize,
}
firehose!(QueueDepth, "worker.queue_depth");
