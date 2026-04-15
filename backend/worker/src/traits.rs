/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-exports trait abstractions from proto and provides production implementations.

pub use proto::traits::{DrvReader, JobReporter};

use anyhow::Result;
use async_trait::async_trait;

/// Production [`DrvReader`] that reads from the filesystem.
pub struct FsDrvReader;

#[async_trait]
impl DrvReader for FsDrvReader {
    async fn read_drv(&self, store_path: &str) -> Result<Vec<u8>> {
        let full_path = if store_path.starts_with('/') {
            store_path.to_string()
        } else {
            format!("/nix/store/{}", store_path)
        };
        let bytes = tokio::fs::read(&full_path).await?;
        Ok(bytes)
    }
}
