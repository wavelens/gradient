/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct JobPhaseView {
    pub seq: i32,
    pub parent_seq: Option<i32>,
    pub phase: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub paths: i32,
    pub bytes: i64,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct FlakeGraphNode {
    pub path: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_derivation: bool,
    pub drv_path: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, utoipa::ToSchema)]
pub struct BoardProject {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
}
