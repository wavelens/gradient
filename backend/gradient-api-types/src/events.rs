/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BuildProgress {
    pub phase: BuildProgressPhase,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub paths_done: u32,
    pub paths_total: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildProgressPhase {
    Download,
    Prefetch,
    Upload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvaluationProgress {
    Fetching { inputs: Vec<InputFetch> },
    Evaluating { thunks: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InputFetch {
    pub name: String,
    pub state: InputFetchState,
    pub downloaded_bytes: u64,
    pub expected_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum InputFetchState {
    Queued,
    Fetching,
    Done,
    Failed,
}
