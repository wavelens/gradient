/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::EventKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    UploadSessions,
    BuildRequestBlobs,
    Evaluations,
    StaleCachedPaths,
    OrphanNars,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Swept {
    pub pass: Pass,
    pub removed: u64,
}

impl EventKind for Swept {
    const NAME: &'static str = "gc.swept";
    const DURABLE: bool = true;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeepFinished {
    pub report: serde_json::Value,
}

impl EventKind for DeepFinished {
    const NAME: &'static str = "gc.deep_finished";
    const DURABLE: bool = true;
}
