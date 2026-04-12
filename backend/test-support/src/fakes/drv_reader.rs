/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fake [`DrvReader`] that serves `.drv` bytes from memory.
//!
//! Typically populated from [`StoreFixture::raw_drvs`].

use anyhow::Result;
use async_trait::async_trait;
use proto::traits::DrvReader;
use std::collections::HashMap;

/// In-memory [`DrvReader`] backed by a map of store paths to raw bytes.
#[derive(Debug, Default)]
pub struct FakeDrvReader {
    drvs: HashMap<String, Vec<u8>>,
}

impl FakeDrvReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from `StoreFixture.raw_drvs`.
    pub fn from_raw_drvs(drvs: HashMap<String, Vec<u8>>) -> Self {
        Self { drvs }
    }

    pub fn with_drv(mut self, store_path: impl Into<String>, data: Vec<u8>) -> Self {
        self.drvs.insert(store_path.into(), data);
        self
    }
}

#[async_trait]
impl DrvReader for FakeDrvReader {
    async fn read_drv(&self, store_path: &str) -> Result<Vec<u8>> {
        // Normalize: ensure full /nix/store/ path for lookup.
        let key = if store_path.starts_with('/') {
            store_path.to_string()
        } else {
            format!("/nix/store/{}", store_path)
        };

        self.drvs
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("FakeDrvReader: no drv for {}", key))
    }
}
