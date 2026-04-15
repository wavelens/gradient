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
    updater: &mut JobUpdater,
) -> Result<()> {
    updater.report_building(task.build_id.clone())?;

    let full_drv_path = nix_store_path(&task.drv_path);
    debug!(drv = %full_drv_path, "building derivation locally");

    // ── Parse .drv file ───────────────────────────────────────────────────────
    let drv_bytes = tokio::fs::read(&full_drv_path)
        .await
        .with_context(|| format!("read .drv file: {}", full_drv_path))?;

    let drv =
        parse_drv(&drv_bytes).with_context(|| format!("parse .drv file: {}", full_drv_path))?;

    // ── Build BasicDerivation for harmonia ────────────────────────────────────
    // Output paths are taken directly from the .drv file.
    // Sending Deferred outputs causes the daemon to return CA paths like
    // `sha256:hash-name` in its response, which harmonia cannot parse.
    let harmonia_path = StorePath::from_base_path(strip_store_prefix(&full_drv_path))
        .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", full_drv_path, e))?;

    let basic_drv = get_basic_derivation(&full_drv_path, &drv)?;

    let mut guard = store
        .pool()
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire local store for build: {}", e))?;

    debug!(
        drv = %task.drv_path,
        platform = %drv.system,
        builder = %drv.builder,
        outputs = ?drv.outputs.iter().map(|o| &o.name).collect::<Vec<_>>(),
        input_drvs = drv.input_derivations.len(),
        input_srcs = drv.input_sources.len(),
        env_keys = ?drv.environment.keys().collect::<Vec<_>>(),
        "sending BasicDerivation to nix-daemon"
    );

    let result = guard
        .client()
        .build_derivation(&harmonia_path, &basic_drv, BuildMode::Normal)
        .await
        .map_err(|e| anyhow::anyhow!(
            "build_derivation failed for {} (platform={}, builder={}, outputs=[{}], env_keys=[{}]): {}",
            task.drv_path,
            drv.system,
            drv.builder,
            drv.outputs.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", "),
            drv.environment.keys().cloned().collect::<Vec<_>>().join(", "),
            e,
        ))?;

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

    updater.report_build_output(task.build_id.clone(), outputs)
}

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
fn get_basic_derivation(
    full_drv_path: &str,
    drv: &gradient_core::db::Derivation,
) -> Result<BasicDerivation> {
    // ── Build outputs from .drv data ──────────────────────────────────────────
    let mut outputs: BTreeMap<_, _> = BTreeMap::new();
    for o in &drv.outputs {
        let output_name = o
            .name
            .parse()
            .with_context(|| format!("invalid output name '{}' in {}", o.name, full_drv_path))?;
        let drv_output = if o.path.is_empty() {
            DerivationOutput::Deferred
        } else {
            let base = strip_nix_store_prefix(o.path.as_str());
            let sp = StorePath::from_base_path(&base).with_context(|| {
                format!("invalid output path '{}' in {}", o.path, full_drv_path)
            })?;
            DerivationOutput::InputAddressed(sp)
        };
        outputs.insert(output_name, drv_output);
    }

    // ── Input sources (no .drv paths — those are already built) ──────────────
    let inputs: harmonia_store_core::store_path::StorePathSet = drv
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

    // ── Structured attributes ─────────────────────────────────────────────────
    // harmonia's NixSerialize for BasicDerivation never writes `structured_attrs`
    // to the wire — only `env` is sent. The `__json` key in env is what the Nix
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
