/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Construct a harmonia [`BasicDerivation`] from a parsed `.drv` file.

use anyhow::{Context, Result};
use bytes::Bytes;
use gradient_db::{DrvOutputSpec, parse_drv};
use gradient_exec::path_utils::{nix_store_path, strip_nix_store_prefix};
use harmonia_store_content_address::{ContentAddress, ContentAddressMethod};
use harmonia_store_derivation::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_path::StorePath;
use harmonia_utils_hash::{Algorithm, Hash};
use std::collections::BTreeMap;
use tracing::warn;

/// Construct a harmonia [`BasicDerivation`] from a parsed drv file.
///
/// Output paths are taken directly from the `.drv` file:
/// - non-empty `path` → `InputAddressed` (concrete store path)
/// - empty `path` → `Deferred` (floating CA derivation)
///
/// This avoids calling `query_derivation_output_map`, which fails on some
/// daemon versions that return full `/nix/store/...` paths where harmonia
/// expects bare `hash-name` paths.
///
/// Structured attributes (`__json`) are moved from the env map to
/// `structured_attrs` so the daemon handles them correctly.
pub(super) async fn get_basic_derivation(
    full_drv_path: &str,
    drv: &gradient_db::Derivation,
) -> Result<BasicDerivation> {
    // ── Build outputs from .drv data ──────────────────────────────────────────
    //
    // Three shapes of `.drv` output to disambiguate:
    //
    // 1. Fixed-output derivation (FOD): both `hash_algo` and `hash` are
    //    populated (e.g. a `fetchurl`). The daemon **must** see this as
    //    `CAFixed(ContentAddress)` because that's what unlocks network
    //    access in the build sandbox - passing it as `InputAddressed`
    //    sandboxes it without DNS, so curl fails with
    //    `Could not resolve host: …` and the build dies.
    // 2. Floating CA derivation: `path` is empty AND `hash_algo` is empty.
    //    Daemon will compute the path from the build output → `Deferred`.
    // 3. Plain input-addressed derivation: `path` is set, no `hash_algo`.
    //    → `InputAddressed(StorePath)`.
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

    // ── Input paths: input_sources + output paths of input_derivations ────────
    // The daemon needs all direct inputs present in the store before building.
    // input_sources are plain store paths; input_derivations map drv→outputs,
    // so we read each input .drv to resolve the concrete output paths.
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

    for (input_drv_path, _output_names) in &drv.input_derivations {
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

        for o in &input_drv.outputs {
            if o.path.is_empty() {
                continue;
            }

            let base = strip_nix_store_prefix(&o.path);
            match StorePath::from_base_path(&base) {
                Ok(sp) => {
                    inputs.insert(sp);
                }

                Err(e) => {
                    warn!(path = %o.path, error = %e, "skipping input drv output: not a valid store path");
                }
            }
        }
    }

    // ── Structured attributes ─────────────────────────────────────────────────
    // harmonia's NixSerialize for BasicDerivation never writes `structured_attrs`
    // to the wire - only `env` is sent. The `__json` key in env is what the Nix
    // daemon reads for structured-attrs derivations, so leave it in place.

    // Extract the name from the drv path ("hash-name.drv" → "name.drv").
    let base = strip_nix_store_prefix(full_drv_path);
    let drv_name = base
        .find('-')
        .map(|i| base[i + 1..].to_owned())
        .unwrap_or_else(|| base.to_owned());

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
        structured_attrs: None,
    })
}

/// Build a `DerivationOutput::CAFixed(...)` from a `.drv`'s `outputHashAlgo`
/// and `outputHash` fields. Without this the daemon would treat the FOD as
/// an input-addressed derivation, sandbox it without network access, and
/// every fetch (curl, git clone, …) would fail with DNS errors.
///
/// The `.drv` `hash_algo` field follows Nix's wire format:
///   `"sha256"`        → flat sha256
///   `"r:sha256"`      → recursive (NAR-hashed) sha256
///   `"text:sha256"`   → text-hashed (rare, used by `builtins.toFile`)
///
/// The `hash` field is hex-encoded (base16) raw digest bytes.
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
    fn ca_fixed_flat_sha256() {
        // sha256("hello") in hex
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
        // sha256 needs 32 bytes (64 hex chars); pass 8 bytes (16 hex chars).
        assert!(ca_fixed_output("sha256", "deadbeefdeadbeef").is_err());
    }
}
