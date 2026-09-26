/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::firehose;
use crate::ids::{CachedPathId, EvaluationId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ingested {
    pub evaluation_id: EvaluationId,
    pub task: Option<TaskId>,
    pub walked: usize,
    pub entry_points: usize,
    pub skipped: bool,
}
firehose!(Ingested, "graph.ingested");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarCommitted {
    pub cached_path: CachedPathId,
    pub created: bool,
    pub outputs_marked: u64,
}
firehose!(NarCommitted, "graph.nar_committed");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transitioned {
    pub aborted: usize,
    pub prioritized: usize,
}
firehose!(Transitioned, "graph.transitioned");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Requeued {
    pub requeued: u64,
}
firehose!(Requeued, "graph.requeued");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Demoted {
    pub producers: usize,
    pub others_remain: bool,
}
firehose!(Demoted, "graph.demoted");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Collected {
    pub derivations: usize,
    pub evaluations: usize,
    pub retired_paths: usize,
}
firehose!(Collected, "graph.collected");
