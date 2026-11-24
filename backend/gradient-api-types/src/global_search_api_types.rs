/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Project,
    Task,
    Cache,
    Nar,
    Commit,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct SearchHit {
    pub kind: HitKind,
    pub label: String,
    pub sublabel: String,
    pub route: String,
    pub starred: bool,
}
