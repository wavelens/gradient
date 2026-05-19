/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared request/response shapes used by more than one build-request
//! endpoint. Kept here so `manifest`, `blobs`, and `dispatch` don't have to
//! cross-import each other.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: i64,
}
