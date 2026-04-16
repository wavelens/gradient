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
use futures::StreamExt as _;
use gradient_core::db::parse_drv;
use gradient_core::executer::path_utils::{nix_store_path, strip_nix_store_prefix};
use gradient_core::sources::get_hash_from_path;
use harmonia_protocol::build_result::BuildResultInner;
use harmonia_protocol::daemon_wire::types2::BuildMode;
use harmonia_protocol::log::{Field, LogMessage, ResultType};
use harmonia_store_core::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use proto::messages::{BuildOutput, BuildTask};
use std::collections::BTreeMap;
use std::pin::pin;
use tracing::{debug, info, warn};

use crate::nix::store::{LocalNixStore, strip_store_prefix};
use crate::proto::job::JobUpdater;

/// Build a single derivation on the local nix-daemon.
///
/// Reports [`JobUpdateKind::Building`] at start and
/// [`JobUpdateKind::BuildOutput`] with the realised outputs on success.
/// Streams build log lines to the server via `LogChunk` messages while the
/// daemon is running.
pub async fn build_derivation(
    store: &LocalNixStore,
    task: &BuildTask,
    task_index: u32,
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

    // ── Drive the daemon and stream logs back to the server ──────────────────
    // `build_derivation` returns `impl ResultLog = Stream<Item = LogMessage> + Future`.
    // We must consume the stream first; only then is the future ready with the
    // BuildResult. Forward stdout/stderr-bearing log messages to the server as
    // `LogChunk` frames so they end up in the build's log_storage.
    let logs = guard
        .client()
        .build_derivation(&harmonia_path, &basic_drv, BuildMode::Normal);
    let mut logs = pin!(logs);
    while let Some(msg) = logs.next().await {
        if let Some(line) = log_message_to_text(&msg)
            && let Err(e) = updater.send_log_chunk(task_index, line.into_bytes())
        {
            // Log streaming is best-effort — never fail the build because the
            // server connection hiccupped.
            warn!(error = %e, "failed to forward build log chunk; continuing");
        }
    }
    let result = logs.await.map_err(|e| {
        anyhow::anyhow!("build_derivation failed for {}: {}", task.drv_path, e)
    })?;

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

/// Extract a forwardable log line from a harmonia daemon log message.
///
/// Returns:
/// - `Message`: the high-level message text (errors, warnings, status notes).
/// - `BuildLogLine`/`PostBuildLogLine` results: the raw stdout/stderr line
///   from the build sandbox or post-build hook (the actual build log).
///
/// All other variants (StartActivity / StopActivity / progress results) are
/// noisy structured events that don't belong in the user-facing build log.
fn log_message_to_text(msg: &LogMessage) -> Option<String> {
    match msg {
        LogMessage::Message(m) => {
            let mut s = String::from_utf8_lossy(&m.text).into_owned();
            s.push('\n');
            Some(s)
        }
        LogMessage::Result(r)
            if matches!(
                r.result_type,
                ResultType::BuildLogLine | ResultType::PostBuildLogLine
            ) =>
        {
            // BuildLogLine/PostBuildLogLine results carry the line as the first String field.
            r.fields.iter().find_map(|f| match f {
                Field::String(b) => {
                    let mut s = String::from_utf8_lossy(b).into_owned();
                    s.push('\n');
                    Some(s)
                }
                _ => None,
            })
        }
        _ => None,
    }
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

    // ── Input paths: input_sources + output paths of input_derivations ────────
    // The daemon needs all direct inputs present in the store before building.
    // input_sources are plain store paths; input_derivations map drv→outputs,
    // so we read each input .drv to resolve the concrete output paths.
    let mut inputs: harmonia_store_core::store_path::StorePathSet = drv
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
        let input_bytes = match std::fs::read(&input_full) {
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
                Ok(sp) => { inputs.insert(sp); }
                Err(e) => {
                    warn!(path = %o.path, error = %e, "skipping input drv output: not a valid store path");
                }
            }
        }
    }

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
