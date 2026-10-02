/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use anyhow::Result;
use gradient_derivation::parse_drv;
use gradient_wire::messages::CachedPath;
use gradient_worker_client::compression::{Compression, decompress, extract_single_file_from_nar};
use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
use harmonia_store_path::{StoreDir, StorePath};
use harmonia_utils_hash::Hash;
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_signature::Signature;
use tracing::warn;

/// Seeds are re-derived from the `.drv` text rather than taken from `cached_path.references` alone.
/// The eval worker can store `NULL` references after a failed `gather_path_meta` query. The daemon
/// would then reject the `.drv` import with `path '...' is not valid`.
pub(crate) fn drv_closure_seeds(drv: &gradient_derivation::Derivation) -> Vec<String> {
    drv.input_derivations
        .iter()
        .map(|(drv_path, _)| drv_path)
        .chain(&drv.input_sources)
        .cloned()
        .collect()
}

pub(crate) async fn drv_closure_seeds_from_compressed_nar(
    compressed: &[u8],
    compression: Compression,
    drv_path: &str,
) -> Vec<String> {
    let owned = compressed.to_vec();
    let nar = match tokio::task::spawn_blocking(move || decompress(&owned, compression)).await {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            warn!(drv = %drv_path, error = %e, "decompress failed while harvesting drv closure seeds");
            return Vec::new();
        }
        Err(e) => {
            warn!(drv = %drv_path, error = %e, "decompress task panicked while harvesting drv closure seeds");
            return Vec::new();
        }
    };
    let drv_bytes = match extract_single_file_from_nar(&nar).await {
        Ok(b) => b,
        Err(e) => {
            warn!(drv = %drv_path, error = %e, "could not extract drv file from NAR");
            return Vec::new();
        }
    };
    let drv = match parse_drv(&drv_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!(drv = %drv_path, error = %e, "could not parse fetched .drv");
            return Vec::new();
        }
    };
    drv_closure_seeds(&drv)
}

pub(crate) fn build_unkeyed_path_info(
    store_path: &str,
    meta: &CachedPath,
    nar_size: u64,
) -> Result<UnkeyedValidPathInfo> {
    let nar_hash_str = meta
        .nar_hash
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cache metadata missing nar_hash for {}", store_path))?;

    let hash_any = nar_hash_str
        .parse::<Any<Hash>>()
        .map_err(|e| anyhow::anyhow!("parse nar_hash '{}': {}", nar_hash_str, e))?;

    let nar_hash = hash_any
        .into_hash()
        .try_into()
        .map_err(|e| anyhow::anyhow!("convert nar_hash '{}' to NarHash: {}", nar_hash_str, e))?;

    let mut references: BTreeSet<StorePath> = BTreeSet::new();
    if let Some(refs) = meta.references.as_ref() {
        for r in refs {
            let base = r.strip_prefix("/nix/store/").unwrap_or(r);
            match StorePath::from_base_path(base) {
                Ok(sp) => {
                    references.insert(sp);
                }
                Err(e) => {
                    warn!(reference = %r, error = %e, "skipping invalid reference");
                }
            }
        }
    }

    let deriver = meta.deriver.as_ref().and_then(|d| {
        let base = d.strip_prefix("/nix/store/").unwrap_or(d);
        match StorePath::from_base_path(base) {
            Ok(sp) => Some(sp),
            Err(e) => {
                warn!(deriver = %d, error = %e, "skipping invalid deriver");
                None
            }
        }
    });

    let mut signatures: BTreeSet<Signature> = BTreeSet::new();
    if let Some(sigs) = meta.signatures.as_ref() {
        for s in sigs {
            match s.parse::<Signature>() {
                Ok(sig) => {
                    signatures.insert(sig);
                }
                Err(e) => {
                    warn!(signature = %s, error = %e, "skipping unparseable signature");
                }
            }
        }
    }

    let ca = meta.ca.as_ref().and_then(|c| match c.parse() {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            warn!(ca = %c, "skipping unparseable content-address");
            None
        }
    });

    Ok(UnkeyedValidPathInfo {
        deriver,
        nar_hash,
        references,
        registration_time: None,
        nar_size,
        ultimate: false,
        signatures,
        ca,
        store_dir: StoreDir::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_unkeyed_minimal_meta() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(123),
            url: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        let info = build_unkeyed_path_info(&meta.path, &meta, 123).unwrap();
        assert_eq!(info.nar_size, 123);
        assert!(info.references.is_empty());
        assert!(info.signatures.is_empty());
        assert!(info.deriver.is_none());
        assert!(info.ca.is_none());
        assert!(!info.ultimate);
    }

    #[test]
    fn build_unkeyed_collects_references_and_signatures() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: None,
            references: Some(vec![
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-y".into(),
                "/nix/store/cccccccccccccccccccccccccccccccc-z".into(),
            ]),
            signatures: Some(vec![
                "cache.example.com-1:tooShort".into(),
                "garbage-no-colon".into(),
            ]),
            deriver: Some("/nix/store/dddddddddddddddddddddddddddddddd-x.drv".into()),
            ca: None,
        };
        let info = build_unkeyed_path_info(&meta.path, &meta, 0).unwrap();
        assert_eq!(info.references.len(), 2);
        assert!(info.deriver.is_some());
        assert_eq!(info.signatures.len(), 0);
    }

    #[test]
    fn missing_nar_hash_is_an_error() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        assert!(build_unkeyed_path_info(&meta.path, &meta, 0).is_err());
    }

    #[test]
    fn drv_closure_seeds_are_inputs_never_outputs() {
        use gradient_derivation::parse_drv;

        let drv_bytes = br#"Derive([("debug","/nix/store/dddddddddddddddddddddddddddddddd-debug","",""),("out","/nix/store/oooooooooooooooooooooooooooooooo-out","","")],[("/nix/store/iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-dep.drv",["out"])],["/nix/store/ssssssssssssssssssssssssssssssss-src.sh"],"x86_64-linux","/bin/sh",[],[])"#;
        let drv = parse_drv(drv_bytes).unwrap();

        assert_eq!(
            drv_closure_seeds(&drv),
            vec![
                "/nix/store/iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-dep.drv".to_string(),
                "/nix/store/ssssssssssssssssssssssssssssssss-src.sh".to_string(),
            ]
        );
    }
}
