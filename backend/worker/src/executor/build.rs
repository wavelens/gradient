/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build task — invoke the local nix-daemon to build a single derivation.
//!
//! Unlike the server's `SshBuildExecutor`, the worker builds directly against
//! its own local nix-daemon (no SSH tunneling). Dependencies are already
//! present in the local store (placed there by the server via NarPush or S3).

use anyhow::{Context, Result};
use bytes::Bytes;
use gradient_core::db::parse_drv;
use gradient_core::executer::path_utils::{nix_store_path, strip_nix_store_prefix};
use gradient_core::sources::get_hash_from_path;
use harmonia_protocol::build_result::BuildResultInner;
use harmonia_protocol::daemon_wire::types2::BuildMode;
use harmonia_store_core::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_core::store_path::ContentAddress;
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use proto::messages::{BuildOutput, BuildTask};
use std::collections::BTreeMap;
use tracing::{debug, info, warn};

use crate::nix::store::{LocalNixStore, strip_store_prefix};
use crate::proto::job::JobUpdater;

/// Build a single derivation on the local nix-daemon.
///
/// Reports [`JobUpdateKind::Building`] at start and
/// [`JobUpdateKind::BuildOutput`] with the realised outputs on success.
pub async fn build_derivation(
    store: &LocalNixStore,
    task: &BuildTask,
    updater: &mut JobUpdater<'_>,
) -> Result<()> {
    updater.report_building(task.build_id.clone()).await?;

    let full_drv_path = nix_store_path(&task.drv_path);
    debug!(drv = %full_drv_path, "building derivation locally");

    // ── Parse .drv file ───────────────────────────────────────────────────────
    let drv_bytes = tokio::fs::read(&full_drv_path)
        .await
        .with_context(|| format!("read .drv file: {}", full_drv_path))?;

    let drv =
        parse_drv(&drv_bytes).with_context(|| format!("parse .drv file: {}", full_drv_path))?;

    // ── Build BasicDerivation for harmonia ────────────────────────────────────
    let basic_drv = build_basic_derivation(&task.drv_path, &drv)?;

    // ── Call local nix-daemon ─────────────────────────────────────────────────
    let harmonia_path = StorePath::from_base_path(strip_store_prefix(&full_drv_path))
        .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", full_drv_path, e))?;

    let mut guard = store
        .pool()
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire local store for build: {}", e))?;

    let result = guard
        .client()
        .build_derivation(&harmonia_path, &basic_drv, BuildMode::Normal)
        .await
        .map_err(|e| anyhow::anyhow!("build_derivation failed for {}: {}", task.drv_path, e))?;

    // ── Process build result ──────────────────────────────────────────────────
    let outputs = match &result.inner {
        BuildResultInner::Success(s) => {
            info!(drv = %task.drv_path, "build succeeded");
            let mut out = Vec::with_capacity(s.built_outputs.len());
            for (output_name, realisation) in &s.built_outputs {
                let store_path_str = format!("/nix/store/{}", realisation.out_path);
                let (hash, _package) = get_hash_from_path(store_path_str.clone())
                    .with_context(|| format!("parse output path: {}", store_path_str))?;
                let has_artefacts = tokio::fs::metadata(format!(
                    "{}/nix-support/hydra-build-products",
                    store_path_str
                ))
                .await
                .is_ok();
                out.push(BuildOutput {
                    name: output_name.to_string(),
                    store_path: store_path_str,
                    hash,
                    nar_size: None, // filled in by compress step
                    nar_hash: None,
                    has_artefacts,
                });
            }
            out
        }
        BuildResultInner::Failure(f) => {
            let msg = String::from_utf8_lossy(&f.error_msg);
            warn!(drv = %task.drv_path, error = %msg, "build failed");
            return Err(anyhow::anyhow!("build failed: {}", msg));
        }
    };

    updater
        .report_build_output(task.build_id.clone(), outputs)
        .await
}

/// Construct a harmonia [`BasicDerivation`] from a parsed drv file.
fn build_basic_derivation(
    drv_path: &str,
    drv: &gradient_core::db::Derivation,
) -> Result<BasicDerivation> {
    let mut outputs: BTreeMap<_, _> = BTreeMap::new();
    for o in &drv.outputs {
        let output_name = o
            .name
            .parse()
            .with_context(|| format!("invalid output name: {}", o.name))?;
        let drv_output = if !o.hash_algo.is_empty() && !o.hash.is_empty() {
            // Fixed-output derivation (FOD): the hash_algo field is either
            // "sha256" (flat) or "r:sha256" (recursive/NAR hash).
            let (recursive, algo_str) = o
                .hash_algo
                .strip_prefix("r:")
                .map(|rest| (true, rest))
                .unwrap_or((false, &o.hash_algo));
            let algorithm: harmonia_utils_hash::Algorithm = algo_str
                .parse()
                .with_context(|| format!("unknown hash algorithm: {}", o.hash_algo))?;
            let hash =
                harmonia_utils_hash::fmt::Base16::parse(algorithm, &o.hash)
                    .or_else(|_| harmonia_utils_hash::fmt::Base32::parse(algorithm, &o.hash))
                    .with_context(|| {
                        format!("invalid hash for output {}: {}", o.name, o.hash)
                    })?;
            let ca = if recursive {
                ContentAddress::Recursive(hash)
            } else {
                ContentAddress::Flat(hash)
            };
            DerivationOutput::CAFixed(ca)
        } else if o.path.is_empty() {
            DerivationOutput::Deferred
        } else {
            let sp = StorePath::from_base_path(strip_store_prefix(&o.path))
                .with_context(|| format!("invalid output store path: {}", o.path))?;
            DerivationOutput::InputAddressed(sp)
        };
        outputs.insert(output_name, drv_output);
    }

    // Input derivation outputs + input sources → flat StorePath set.
    let inputs: harmonia_store_core::store_path::StorePathSet = drv
        .input_derivations
        .iter()
        .map(|(p, _)| p.as_str())
        .chain(drv.input_sources.iter().map(String::as_str))
        .filter_map(|p| {
            let full = nix_store_path(p);
            let base = strip_nix_store_prefix(&full).to_owned();
            StorePath::from_base_path(&base).ok()
        })
        .collect();

    // Extract the name component from the drv path (e.g. "hash-name.drv" → "name.drv").
    let base = strip_nix_store_prefix(drv_path);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::db::{Derivation, DerivationOutput};
    use harmonia_store_core::derivation::DerivationOutput as HarmoniaOutput;

    fn empty_drv() -> Derivation {
        Derivation {
            outputs: vec![],
            input_derivations: vec![],
            input_sources: vec![],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: Default::default(),
        }
    }

    #[test]
    fn build_basic_drv_empty_path_deferred() {
        let mut drv = empty_drv();
        drv.outputs = vec![DerivationOutput {
            name: "out".into(),
            path: "".into(),
            hash_algo: "".into(),
            hash: "".into(),
        }];
        let basic = build_basic_derivation("aaaa-hello.drv", &drv).unwrap();
        let out_name: harmonia_store_core::derived_path::OutputName = "out".parse().unwrap();
        let out = basic
            .outputs
            .get(&out_name)
            .expect("output 'out' not found");
        assert!(
            matches!(out, HarmoniaOutput::Deferred),
            "empty path → Deferred"
        );
    }

    #[test]
    fn build_basic_drv_nonempty_path_input_addressed() {
        let mut drv = empty_drv();
        // nix store path hashes are 32 nix-base32 chars
        drv.outputs = vec![DerivationOutput {
            name: "out".into(),
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello".into(),
            hash_algo: "".into(),
            hash: "".into(),
        }];
        let basic =
            build_basic_derivation("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-hello.drv", &drv).unwrap();
        let out_name: harmonia_store_core::derived_path::OutputName = "out".parse().unwrap();
        let out = basic
            .outputs
            .get(&out_name)
            .expect("output 'out' not found");
        assert!(
            matches!(out, HarmoniaOutput::InputAddressed(_)),
            "non-empty path → InputAddressed"
        );
    }

    #[test]
    fn build_basic_drv_fixed_output_flat() {
        let mut drv = empty_drv();
        // Fixed-output derivation with flat sha256 hash (like fetchurl).
        // 64 hex chars = 32 bytes = sha256.
        drv.outputs = vec![DerivationOutput {
            name: "out".into(),
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source.tar.gz".into(),
            hash_algo: "sha256".into(),
            hash: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        }];
        drv.builder = "builtin:fetchurl".into();
        let basic = build_basic_derivation("aaaa-source.tar.gz.drv", &drv).unwrap();
        let out_name: harmonia_store_core::derived_path::OutputName = "out".parse().unwrap();
        let out = basic.outputs.get(&out_name).expect("output 'out' not found");
        assert!(
            matches!(out, HarmoniaOutput::CAFixed(_)),
            "FOD with sha256 hash → CAFixed, got {:?}",
            out,
        );
    }

    #[test]
    fn build_basic_drv_fixed_output_recursive() {
        let mut drv = empty_drv();
        // Fixed-output derivation with recursive (NAR) sha256 hash.
        drv.outputs = vec![DerivationOutput {
            name: "out".into(),
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source".into(),
            hash_algo: "r:sha256".into(),
            hash: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        }];
        drv.builder = "builtin:fetchurl".into();
        let basic = build_basic_derivation("aaaa-source.drv", &drv).unwrap();
        let out_name: harmonia_store_core::derived_path::OutputName = "out".parse().unwrap();
        let out = basic.outputs.get(&out_name).expect("output 'out' not found");
        match out {
            HarmoniaOutput::CAFixed(ca) => {
                assert!(
                    matches!(ca, ContentAddress::Recursive(_)),
                    "r:sha256 → Recursive, got {:?}",
                    ca,
                );
            }
            other => panic!("expected CAFixed, got {:?}", other),
        }
    }

    #[test]
    fn build_basic_drv_name_extraction() {
        let drv = empty_drv();
        // nix store path hashes are 32 nix-base32 chars
        let basic = build_basic_derivation(
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv",
            &drv,
        )
        .unwrap();
        assert_eq!(basic.name.as_ref(), "hello.drv");
    }

    #[test]
    fn build_basic_drv_no_dash_full_base() {
        // If there's no '-' in the base path, use the full base as the name.
        let drv = empty_drv();
        let basic = build_basic_derivation("nodashname.drv", &drv).unwrap();
        assert_eq!(basic.name.as_ref(), "nodashname.drv");
    }
}
