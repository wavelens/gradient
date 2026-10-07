/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::score::rules::estimated_time::TimeEstimate;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub rules: BTreeMap<String, f64>,
    pub total: f64,
    /// Vetoing rules are blocking this worker regardless of `total`. Rows recorded before vetoes
    /// existed are missing the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vetoes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<TimeEstimate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_breakdown_stored_before_estimates_reads_without_an_estimate() {
        let old: ScoreBreakdown =
            serde_json::from_str(r#"{"rules":{"QosRule":0.0},"total":0.0}"#).unwrap();
        assert_eq!(old.estimate, None);
        assert!(
            !serde_json::to_string(&old).unwrap().contains("estimate"),
            "no estimate, no key"
        );
    }
}
