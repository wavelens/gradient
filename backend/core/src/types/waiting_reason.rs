/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Structured "why is this evaluation waiting?" payload.
//!
//! Persisted on `evaluation.waiting_reason` (JSON) by the scheduler whenever it
//! reconciles an evaluation against the connected worker pool, returned by the
//! `GET /evals/{evaluation}` endpoint, and rendered by the frontend's
//! "Waiting for Workers" panel.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingReason {
    pub unmet: Vec<UnmetRequirement>,
    pub connected_workers: u32,
    pub available_architectures: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmetRequirement {
    pub architecture: String,
    pub required_features: Vec<String>,
    pub build_count: u32,
}

impl WaitingReason {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}
