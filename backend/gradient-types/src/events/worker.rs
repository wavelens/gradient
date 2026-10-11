/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{firehose, names};
use crate::ids::{DerivationBuildId, EvaluationId, ProjectId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connected {
    pub worker_id: String,
    pub projects: Vec<ProjectId>,
}

firehose!(Connected, names::WORKER_CONNECTED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Disconnected {
    pub worker_id: String,
}

firehose!(Disconnected, names::WORKER_DISCONNECTED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobDispatched {
    pub project: ProjectId,
    pub worker_id: String,
    pub kind: i16,
    pub score: f64,
    pub build_id: Option<DerivationBuildId>,
    pub evaluation_id: EvaluationId,
}
firehose!(JobDispatched, names::WORKER_JOB_DISPATCHED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueDepth {
    pub workers: usize,
    pub pending: usize,
    pub active: usize,
}
firehose!(QueueDepth, names::WORKER_QUEUE_DEPTH);
