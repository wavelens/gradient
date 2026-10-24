/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_derivation::Derivation;
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

pub fn stored_derivation(
    mapper: &StorePathMapper,
    mut drv: Derivation,
) -> anyhow::Result<Derivation> {
    for output in drv.outputs.iter_mut().filter(|o| !o.path.is_empty()) {
        output.path = mapper.inbound_text(&output.path)?;
    }
    for input in &mut drv.input_derivations {
        input.path = mapper.inbound_text(&input.path)?;
    }
    for source in &mut drv.input_sources {
        *source = mapper.inbound_text(source)?;
    }
    Ok(drv)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_derivation::{DerivationOutput, InputDrv};
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_types::CacheId;
    use std::sync::Arc;

    #[test]
    fn a_derivation_read_from_the_cache_names_stored_paths() {
        let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mapper = StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
        let output = |path: String| DerivationOutput {
            name: "out".into(),
            path,
            hash_algo: String::new(),
            hash: String::new(),
        };
        let drv = Derivation {
            outputs: vec![output(format!("/nix/store/{o}-hello"))],
            input_derivations: vec![InputDrv::new(
                format!("/nix/store/{o}-dep.drv"),
                vec!["out".into()],
            )],
            input_sources: vec![format!("/nix/store/{o}-src")],
            system: "x86_64-linux".into(),
            builder: format!("/nix/store/{o}-bash/bin/bash"),
            args: Vec::new(),
            environment: Default::default(),
        };

        let stored = stored_derivation(&mapper, drv).expect("stored");

        assert_eq!(
            stored.outputs,
            vec![output(format!("/nix/store/{s}-hello"))]
        );
        assert_eq!(
            stored.input_derivations,
            vec![InputDrv::new(
                format!("/nix/store/{s}-dep.drv"),
                vec!["out".to_owned()]
            )]
        );
        assert_eq!(stored.input_sources, vec![format!("/nix/store/{s}-src")]);
        assert_eq!(stored.builder, format!("/nix/store/{o}-bash/bin/bash"));
    }
}
