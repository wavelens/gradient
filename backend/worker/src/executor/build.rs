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
use harmonia_protocol::log::{Field, LogMessage, ResultType, Verbosity};
use harmonia_protocol::types::ClientOptions;
use harmonia_store_core::derivation::{BasicDerivation, DerivationOutput, DerivationT};
use harmonia_store_core::store_path::{ContentAddress, ContentAddressMethod, StorePath};
use harmonia_utils_hash::{Algorithm, Hash};
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
    // NOTE: `report_building` is now sent by the caller
    // (`execute_build_job`) *before* the prefetch step, so that a `JobFailed`
    // arriving after a prefetch error finds the build already in `Building`
    // state on the server. Re-sending it here would only generate a
    // redundant `Building → Building` no-op transition + extra webhook.

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

    // ── Crank up daemon verbosity so it actually emits BuildLogLine ──────────
    // Default `ClientOptions::verbose_build` is `Verbosity::Error`, which
    // makes the daemon suppress per-line build stdout/stderr (BuildLogLine
    // results, type 101). Without this every build log stream comes back
    // with only structured Progress / SetPhase / activity events and the
    // user sees an empty build log. Talkative (4) is the level `nix-build`
    // uses by default for showing build output.
    let mut opts = ClientOptions::default();
    opts.verbose_build = Verbosity::Talkative;
    opts.verbosity = Verbosity::Notice;
    if let Err(e) = guard.client().set_options(&opts).await {
        warn!(
            error = %e,
            "set_options(verbose_build=Talkative) failed; build logs may be empty"
        );
    }

    // ── Drive the daemon and stream logs back to the server ──────────────────
    // `build_derivation` returns `impl ResultLog = Stream<Item = LogMessage> + Future`.
    // We must consume the stream first; only then is the future ready with the
    // BuildResult. Forward stdout/stderr-bearing log messages to the server as
    // `LogChunk` frames so they end up in the build's log_storage.
    let logs = guard
        .client()
        .build_derivation(&harmonia_path, &basic_drv, BuildMode::Normal);
    let mut logs = pin!(logs);
    let mut total_msgs: u64 = 0;
    let mut forwarded_lines: u64 = 0;
    let mut forwarded_bytes: u64 = 0;
    let mut send_failures: u64 = 0;
    while let Some(msg) = logs.next().await {
        total_msgs += 1;
        if let Some(line) = log_message_to_text(&msg) {
            let len = line.len();
            match updater.send_log_chunk(task_index, line.into_bytes()) {
                Ok(()) => {
                    forwarded_lines += 1;
                    forwarded_bytes += len as u64;
                }
                Err(e) => {
                    // Log streaming is best-effort — never fail the build
                    // because the server connection hiccupped.
                    send_failures += 1;
                    warn!(error = %e, "failed to forward build log chunk; continuing");
                }
            }
        }
    }
    info!(
        drv = %task.drv_path,
        daemon_messages = total_msgs,
        forwarded_lines,
        forwarded_bytes,
        send_failures,
        "build log stream drained"
    );
    if total_msgs == 0 {
        warn!(
            drv = %task.drv_path,
            "daemon emitted zero LogMessages during build — daemon verbosity may be too low \
             (set `verbose-builds = true` and `log-lines = 0` in nix.conf, or check \
             the worker user's permissions to read daemon output)"
        );
    } else if forwarded_lines == 0 {
        warn!(
            drv = %task.drv_path,
            daemon_messages = total_msgs,
            "daemon emitted LogMessages but none had forwardable text content \
             (only structured progress / activity events) — set `verbose-builds = true` \
             on the worker's nix-daemon to enable BuildLogLine results"
        );
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
/// Captures:
/// - `Message`: high-level messages (errors, warnings, status notes).
/// - `StartActivity`: activity descriptions ("building '/nix/store/…'",
///   "copying '/nix/store/…'" etc.). Useful for builtins (fetchurl, path)
///   that run inside the daemon rather than in a sandbox and therefore never
///   produce `BuildLogLine` results.
/// - `BuildLogLine`/`PostBuildLogLine` results: the raw stdout/stderr lines
///   from the build sandbox or post-build hook (the actual build log).
///
/// `StopActivity`, `Progress`, `SetExpected`, `SetPhase`, and other
/// structured result types are skipped — they're progress-bar bookkeeping,
/// not user-facing log content.
fn log_message_to_text(msg: &LogMessage) -> Option<String> {
    match msg {
        LogMessage::Message(m) => {
            let s = String::from_utf8_lossy(&m.text);
            if s.is_empty() {
                return None;
            }
            Some(format!("{s}\n"))
        }
        LogMessage::StartActivity(a) => {
            let s = String::from_utf8_lossy(&a.text);
            if s.is_empty() {
                return None;
            }
            Some(format!("{s}\n"))
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
                    let s = String::from_utf8_lossy(b);
                    if s.is_empty() {
                        return None;
                    }
                    Some(format!("{s}\n"))
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
    //
    // Three shapes of `.drv` output to disambiguate:
    //
    // 1. Fixed-output derivation (FOD): both `hash_algo` and `hash` are
    //    populated (e.g. a `fetchurl`). The daemon **must** see this as
    //    `CAFixed(ContentAddress)` because that's what unlocks network
    //    access in the build sandbox — passing it as `InputAddressed`
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
        let drv_output = if !o.hash_algo.is_empty() && !o.hash.is_empty() {
            ca_fixed_output(&o.hash_algo, &o.hash).with_context(|| {
                format!(
                    "invalid FOD spec for output '{}' in {} (hash_algo={:?} hash={:?})",
                    o.name, full_drv_path, o.hash_algo, o.hash
                )
            })?
        } else if o.path.is_empty() {
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
        (ContentAddressMethod::Recursive, rest)
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
                assert_eq!(ca.method(), ContentAddressMethod::Recursive);
            }
            other => panic!("expected CAFixed(Recursive), got {other:?}"),
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

