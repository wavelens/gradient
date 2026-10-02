/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub rules: BTreeMap<String, f64>,
    pub total: f64,
    /// Vetoing rules are blocking this worker regardless of `total`. Rows recorded before vetoes
    /// existed are missing the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vetoes: Vec<String>,
}
