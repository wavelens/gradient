/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use harmonia_store_path::ParseStorePathError;
use harmonia_store_remote::DaemonError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreDaemonError {
    #[error("invalid store path {path}: {cause}")]
    InvalidStorePath {
        path: String,
        cause: ParseStorePathError,
    },
    #[error("query_derivation_output_map failed: {cause}")]
    DerivationOutputMapQueryFailed { cause: DaemonError },
    #[error("query_path_info failed: {cause}")]
    PathInfoQueryFailed { cause: DaemonError },
}
