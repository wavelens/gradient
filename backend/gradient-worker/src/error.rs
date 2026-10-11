/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("job aborted during flake input fetch")]
    FlakeInputFetchAborted,
    #[error("store.has_path failed for {path}: {cause}")]
    StoreHasPathFailed { path: String, cause: anyhow::Error },
}
