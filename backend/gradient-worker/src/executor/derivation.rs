/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use bytes::Bytes;
use gradient_derivation::{DrvOutputSpec, parse_drv};
use gradient_util::store_path::{nix_store_path, strip_nix_store_prefix};
use harmonia_store_content_address::{ContentAddress, ContentAddressMethod};
use harmonia_store_derivation::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_path::StorePath;
use harmonia_utils_hash::{Algorithm, Hash};
use std::collections::BTreeMap;
use tracing::warn;

pub(super) async fn get_basic_derivation(
    full_drv_path: &str,
    drv: &gradient_derivation::Derivation,
) -> Result<BasicDerivation> {
    // The daemon must see a fixed-output derivation as `CAFixed` to grant the build sandbox network
    // access. An FOD sent as `InputAddressed` is failing every fetch with `Could not resolve host`.
    let mut outputs: BTreeMap<_, _> = BTreeMap::new();
    for o in &drv.outputs {
        let output_name = o
            .name
            .parse()
            .with_context(|| format!("invalid output name '{}' in {}", o.name, full_drv_path))?;

        let drv_output = match o.as_spec() {
            DrvOutputSpec::FixedOutput { hash_algo, hash } => ca_fixed_output(hash_algo, hash)
                .with_context(|| {
                    format!(
                        "invalid FOD spec for output '{}' in {} (hash_algo={:?} hash={:?})",
                        o.name, full_drv_path, hash_algo, hash
                    )
                })?,
            DrvOutputSpec::Deferred => DerivationOutput::Deferred,
            DrvOutputSpec::InputAddressed { path } => {
                let base = strip_nix_store_prefix(path);
                let sp = StorePath::from_base_path(&base).with_context(|| {
                    format!("invalid output path '{}' in {}", path, full_drv_path)
                })?;
                DerivationOutput::InputAddressed(sp)
            }
        };

        outputs.insert(output_name, drv_output);
    }

    let mut inputs: harmonia_store_path::StorePathSet = drv
        .input_sources
        .iter()
        .filter_map(|p| {
            let full = nix_store_path(p);
            let base = strip_nix_store_prefix(&full).to_owned();
            match StorePath::from_base_path(&base) {
                Ok(sp) => Some(sp),
                Err(e) => {
                    warn!(path = %p, error = %e, "skipping input_src: not a valid store path");
                    None
                }
            }
        })
        .collect();

    for (input_drv_path, output_names) in &drv.input_derivations {
        let input_full = nix_store_path(input_drv_path);
        let input_bytes = match tokio::fs::read(&input_full).await {
            Ok(b) => b,
            Err(e) => {
                warn!(drv = %input_full, error = %e, "cannot read input .drv for inputs");
                continue;
            }
        };

        let input_drv = match parse_drv(&input_bytes) {
            Ok(d) => d,
            Err(e) => {
                warn!(drv = %input_full, error = %e, "cannot parse input .drv for inputs");
                continue;
            }
        };

        for path in input_drv.requested_output_paths(output_names) {
            match StorePath::from_base_path(&strip_nix_store_prefix(path)) {
                Ok(sp) => {
                    inputs.insert(sp);
                }

                Err(e) => {
                    warn!(path = %path, error = %e, "skipping input drv output: not a valid store path");
                }
            }
        }
    }

    let drv_name = derivation_name(full_drv_path);

    Ok(DerivationT {
        name: drv_name
            .parse()
            .with_context(|| format!("invalid derivation name: {}", drv_name))?,
        outputs,
        inputs,
        platform: Bytes::from(drv.system.clone()),
        builder: Bytes::from(drv.builder.clone()),
        args: drv.args.iter().map(|a| Bytes::from(a.clone())).collect(),
        env: drv
            .environment
            .iter()
            .map(|(k, v)| (Bytes::from(k.clone()), Bytes::from(v.clone())))
            .collect(),
        // harmonia is never writing `structured_attrs` to the wire.
        // The daemon is reading the `__json` key in `env` instead.
        structured_attrs: None,
    })
}

fn derivation_name(full_drv_path: &str) -> String {
    let base = strip_nix_store_prefix(full_drv_path);
    let name = base.split_once('-').map_or(base.as_str(), |(_, name)| name);
    name.strip_suffix(".drv").unwrap_or(name).to_owned()
}

fn ca_fixed_output(hash_algo: &str, hash_hex: &str) -> Result<DerivationOutput> {
    let (method, algo_str) = if let Some(rest) = hash_algo.strip_prefix("r:") {
        (ContentAddressMethod::NixArchive, rest)
    } else if let Some(rest) = hash_algo.strip_prefix("text:") {
        (ContentAddressMethod::Text, rest)
    } else {
        (ContentAddressMethod::Flat, hash_algo)
    };

    let algorithm: Algorithm = algo_str
        .parse()
        .map_err(|e| anyhow::anyhow!("unknown hash algorithm {:?}: {}", algo_str, e))?;

    let hash_bytes = hex::decode(hash_hex)
        .with_context(|| format!("hash field {:?} is not valid hex", hash_hex))?;
    let hash = Hash::from_slice(algorithm, &hash_bytes).with_context(|| {
        format!(
            "hash length {} doesn't match {:?} digest size",
            hash_bytes.len(),
            algorithm
        )
    })?;

    let ca = ContentAddress::from_hash(method, hash)
        .map_err(|e| anyhow::anyhow!("ContentAddress::from_hash failed: {}", e))?;
    Ok(DerivationOutput::CAFixed(ca))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_derivation_name_drops_the_drv_suffix() {
        assert_eq!(
            derivation_name("/nix/store/abc-hello-2.12.drv"),
            "hello-2.12"
        );
    }

    #[test]
    fn ca_fixed_flat_sha256() {
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let out = ca_fixed_output("sha256", h).unwrap();
        match out {
            DerivationOutput::CAFixed(ca) => {
                assert_eq!(ca.method(), ContentAddressMethod::Flat);
                assert_eq!(ca.algorithm(), Algorithm::SHA256);
            }
            other => panic!("expected CAFixed(Flat), got {other:?}"),
        }
    }

    #[test]
    fn ca_fixed_recursive_sha256() {
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let out = ca_fixed_output("r:sha256", h).unwrap();
        match out {
            DerivationOutput::CAFixed(ca) => {
                assert_eq!(ca.method(), ContentAddressMethod::NixArchive);
            }
            other => panic!("expected CAFixed(NixArchive), got {other:?}"),
        }
    }

    #[test]
    fn ca_fixed_text_sha256() {
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let out = ca_fixed_output("text:sha256", h).unwrap();
        assert!(matches!(out, DerivationOutput::CAFixed(_)));
    }

    #[test]
    fn ca_fixed_rejects_garbage_algo() {
        assert!(ca_fixed_output("blake7", "deadbeef").is_err());
    }

    #[test]
    fn ca_fixed_rejects_bad_hex() {
        assert!(ca_fixed_output("sha256", "not-hex").is_err());
    }

    #[test]
    fn ca_fixed_rejects_wrong_length_hash() {
        assert!(ca_fixed_output("sha256", "deadbeefdeadbeef").is_err());
    }
}
