/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{firehose, names};
use crate::ids::{CachedPathId, EvaluationId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    pub evaluation_id: EvaluationId,
    pub task: Option<TaskId>,
    pub walked: usize,
    pub entry_points: usize,
    pub skipped: bool,
}
firehose!(Recorded, "graph.recorded");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarCommitted {
    pub cached_path: CachedPathId,
    pub created: bool,
}
firehose!(NarCommitted, names::GRAPH_NAR_COMMITTED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transitioned {
    pub aborted: usize,
    pub prioritized: usize,
}
firehose!(Transitioned, names::GRAPH_TRANSITIONED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Requeued {
    pub requeued: u64,
}
firehose!(Requeued, names::GRAPH_REQUEUED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Demoted {
    pub producers: usize,
    pub others_remain: bool,
}
firehose!(Demoted, names::GRAPH_DEMOTED);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Collected {
    pub derivations: usize,
    pub evaluations: usize,
    pub retired_paths: usize,
}
firehose!(Collected, names::GRAPH_COLLECTED);
