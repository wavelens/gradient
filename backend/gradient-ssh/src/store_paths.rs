/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::{OriginalStorePath, StoreHash, StorePath, StorePathMapper};
use harmonia_protocol::valid_path_info::ValidPathInfo;
use harmonia_store_path::{StorePath as ClientPath, StorePathHash};
use harmonia_store_path_info::NarHash;

pub fn stored(mapper: &StorePathMapper, path: &ClientPath) -> anyhow::Result<StorePath> {
    Ok(mapper.inbound(&OriginalStorePath::parse(&path.to_string())?))
}

pub fn stored_hash(mapper: &StorePathMapper, hash: &StorePathHash) -> anyhow::Result<String> {
    Ok(mapper
        .inbound_hash(StoreHash::parse(&hash.to_string())?)
        .to_string())
}

pub fn original(mapper: &StorePathMapper, stored: &str) -> anyhow::Result<ClientPath> {
    let original = mapper.outbound_text(stored)?;
    let base = original
        .strip_prefix(gradient_types::STORE_DIR)
        .map_or(original.as_str(), |rest| rest.trim_start_matches('/'));
    ClientPath::from_base_path(base).map_err(|e| anyhow::anyhow!("{base}: {e}"))
}

pub fn original_text(mapper: &StorePathMapper, stored: &str) -> anyhow::Result<String> {
    Ok(mapper.outbound_text(stored)?)
}

pub fn stored_full(mapper: &StorePathMapper, original: &str) -> anyhow::Result<String> {
    Ok(mapper.inbound(&OriginalStorePath::parse(original)?).full())
}

pub struct StoredPathInfo {
    pub path: StorePath,
    pub original: ClientPath,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub nar_hash: NarHash,
    pub nar_size: u64,
}

impl StoredPathInfo {
    pub fn inbound(mapper: &StorePathMapper, info: &ValidPathInfo) -> anyhow::Result<Self> {
        Ok(Self {
            path: stored(mapper, &info.path)?,
            original: info.path.clone(),
            references: info
                .info
                .references
                .iter()
                .map(|r| stored(mapper, r).map(|p| p.base()))
                .collect::<anyhow::Result<_>>()?,
            deriver: info
                .info
                .deriver
                .as_ref()
                .map(|d| stored(mapper, d).map(|p| p.full()))
                .transpose()?,
            ca: info.info.ca.as_ref().map(|c| c.to_string()),
            nar_hash: info.info.nar_hash,
            nar_size: info.info.nar_size,
        })
    }
}
