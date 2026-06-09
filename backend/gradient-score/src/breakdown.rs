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
}
