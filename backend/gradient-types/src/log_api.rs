/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::ids::BuildAttemptId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogChunkMeta {
    pub index: u32,
    pub line_start: u64,
    pub line_count: u32,
    pub byte_start: u64,
    pub byte_len: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogChunkIndex {
    pub total_chunks: u32,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub chunks: Vec<LogChunkMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LogSearchHit {
    pub line_number: u64,
    pub chunk_index: u32,
    pub byte_offset: u64,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BuildAttemptLogRow {
    pub id: BuildAttemptId,
    pub outcome: AttemptOutcome,
    pub reason: Option<AttemptFailureReason>,
    pub worker: Option<String>,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

impl BuildAttemptLogRow {
    pub fn new(attempt: gradient_entity::build_attempt::Model, worker: Option<String>) -> Self {
        Self {
            id: attempt.id,
            outcome: attempt.outcome,
            reason: attempt.reason,
            worker,
            started_at: attempt.build_started_at,
            finished_at: attempt.build_finished_at,
            created_at: attempt.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearchDone {
    pub done: bool,
    pub total_matches: u64,
}
