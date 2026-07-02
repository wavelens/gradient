/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-exports trait abstractions from proto and provides production implementations.

pub use gradient_proto::traits::{DrvReader, JobReporter};

use anyhow::Result;
use async_trait::async_trait;
use gradient_exec::path_utils::nix_store_path;

/// Production [`DrvReader`] that reads from the filesystem.
pub struct FsDrvReader;

#[async_trait]
impl DrvReader for FsDrvReader {
    async fn read_drv(&self, store_path: &str) -> Result<Vec<u8>> {
        let full_path = nix_store_path(store_path);
        let bytes = tokio::fs::read(&full_path).await?;
        Ok(bytes)
    }
}
