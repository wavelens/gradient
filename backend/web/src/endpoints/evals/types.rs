/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeEvaluationRequest {
    pub method: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildItem {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub has_artefacts: bool,
    pub updated_at: chrono::NaiveDateTime,
    pub build_time_ms: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct PaginatedBuilds {
    pub builds: Vec<BuildItem>,
    pub total: usize,
    /// Number of builds with status Building, Queued, Failed, Aborted, or DependencyFailed.
    /// The frontend uses this to know how many pages to pre-fetch so all active builds are
    /// in memory (required for correct log streaming and status-transition detection).
    pub active_count: usize,
}

#[derive(Deserialize, Debug, Default)]
pub struct BuildsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Serialize, Debug)]
pub struct EvaluationResponse {
    pub id: Uuid,
    pub project: Option<Uuid>,
    pub project_name: Option<String>,
    pub project_display_name: Option<String>,
    pub repository: String,
    pub commit: String,
    pub wildcard: String,
    pub status: entity::evaluation::EvaluationStatus,
    pub previous: Option<Uuid>,
    pub next: Option<Uuid>,
    pub created_at: chrono::NaiveDateTime,
    pub error_count: u64,
    pub warning_count: u64,
    pub entry_points: Vec<EntryPointBrief>,
}

/// Compact entry-point representation returned inline on the evaluation.
#[derive(Serialize, Debug)]
pub struct EntryPointBrief {
    pub id: Uuid,
    pub eval: String,
    pub build_status: entity::build::BuildStatus,
}

#[derive(Serialize, Debug)]
pub struct EvaluationMessageResponse {
    pub id: Uuid,
    pub level: entity::evaluation_message::MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<Uuid>,
}

/// `/nix/store/hash-name-version.drv` → `name-version`
pub fn drv_display_name(path: &str) -> String {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let after_hash = filename.split_once('-').map(|x| x.1).unwrap_or(filename);
    after_hash
        .strip_suffix(".drv")
        .unwrap_or(after_hash)
        .to_string()
}
