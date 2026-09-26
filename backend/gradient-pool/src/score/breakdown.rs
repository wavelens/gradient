/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Per-rule scoring contributions for one (job, worker) decision, persisted to
/// `dispatched_job.score_breakdown` for scoring debugging.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub rules: BTreeMap<String, f64>,
    pub total: f64,
    /// Rules that vetoed dispatch this round: the job must not dispatch to
    /// this worker regardless of `total`. Absent (empty) in rows recorded
    /// before vetoes existed, and omitted from the JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vetoes: Vec<String>,
}
