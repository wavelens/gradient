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
use harmonia_store_core::derivation::{BasicDerivation, DerivationOutput, DerivationT, StructuredAttrs};
use harmonia_store_core::derived_path::OutputName;
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
    // Acquire the pool guard first so we can query output paths from the daemon
    // before building — mirroring the old scheduler which always provided
    // concrete InputAddressed paths rather than Deferred/CAFixed variants.
    // Sending Deferred outputs causes the daemon to return CA paths like
    // `sha256:hash-name` in its response, which harmonia cannot parse.
    let harmonia_path = StorePath::from_base_path(strip_store_prefix(&full_drv_path))
        .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", full_drv_path, e))?;

    let mut guard = store
        .pool()
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire local store for build: {}", e))?;

    let basic_drv = get_basic_derivation(&full_drv_path, &harmonia_path, &drv, guard.client()).await?;

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
/// Output paths are queried from the nix-daemon via `query_derivation_output_map`
/// so that every output is always `InputAddressed` with a concrete store path.
/// Sending `Deferred` or `CAFixed` outputs causes the daemon to return CA paths
/// (`sha256:hash-name`) in its build response, which harmonia cannot parse.
///
/// Structured attributes (`__json`) are moved from the env map to
/// `structured_attrs` so the daemon handles them correctly.
async fn get_basic_derivation<R, W>(
    full_drv_path: &str,
    harmonia_path: &StorePath,
    drv: &gradient_core::db::Derivation,
    client: &mut gradient_core::executer::GenericDaemonClient<R, W>,
) -> Result<BasicDerivation>
where
    R: tokio::io::AsyncRead + std::fmt::Debug + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + std::fmt::Debug + Unpin + Send + 'static,
{
    // ── Query concrete output paths from the daemon ───────────────────────────
    let output_map: BTreeMap<OutputName, Option<StorePath>> = client
        .query_derivation_output_map(harmonia_path)
        .await
        .map_err(|e| anyhow::anyhow!("query_derivation_output_map for {}: {}", full_drv_path, e))?;

    let mut outputs: BTreeMap<_, _> = BTreeMap::new();
    for (output_name, sp_opt) in output_map {
        let drv_output = match sp_opt {
            Some(sp) => DerivationOutput::InputAddressed(sp),
            None => DerivationOutput::Deferred,
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
    // The raw `.drv` env may contain `__json` as a plain string. Move it to
    // `structured_attrs` so the daemon handles it correctly, and filter it from
    // the env map (Nix C++ does the same during serialisation).
    let structured_attrs = drv
        .environment
        .get("__json")
        .and_then(|v| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(v).ok())
        .map(|attrs| StructuredAttrs { attrs });

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
            .filter(|(k, _)| k.as_str() != "__json")
            .map(|(k, v)| (Bytes::from(k.clone()), Bytes::from(v.clone())))
            .collect(),
        structured_attrs,
    })
}
