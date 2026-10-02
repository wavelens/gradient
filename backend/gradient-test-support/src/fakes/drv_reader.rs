/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_wire::traits::DrvReader;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct FakeDrvReader {
    drvs: HashMap<String, Vec<u8>>,
}

impl FakeDrvReader {
    pub fn new() -> Self {
        Self::default()
    }

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
