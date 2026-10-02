/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use anyhow::{Context, Result};
use gradient_wire::messages::{FlakeJob, FlakeSource};
use gradient_wire::traits::{JobReporter, WorkerStore};
use tempfile::NamedTempFile;
use tokio::sync::watch;
use tracing::{debug, info, trace, warn};

use super::abort_true;
use crate::proto::credentials::CredentialStore;

pub struct FetchOutcome {
    pub source_path: String,
    pub archived_paths: Vec<String>,
}

#[derive(Clone, Copy)]
struct NixTools<'a> {
    nix: &'a str,
    ssh: &'a str,
    store: &'a dyn WorkerStore,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &CredentialStore,
    store: &dyn WorkerStore,
    binpath_nix: &str,
    binpath_ssh: &str,
    mut abort: watch::Receiver<bool>,
) -> Result<FetchOutcome> {
    if *abort.borrow() {
        anyhow::bail!("job aborted");
    }

    updater.report_fetching().await?;

    let ssh_key = credentials
        .ssh_key()
        .map(|k| String::from_utf8_lossy(k.expose()).to_string());

    let (source_path, flake_root) = match &job.source {
        FlakeSource::Repository { url, commit } => {
            let (url, commit) = (url.clone(), commit.clone());
            debug!(%url, %commit, has_ssh_key = ssh_key.is_some(), "fetching repository");

            let ssh_key_for_clone = ssh_key.clone();
            let commit_for_clone = commit.clone();
            let clone_task = tokio::task::spawn_blocking(move || {
                clone_and_checkout(&url, &commit_for_clone, ssh_key_for_clone.as_deref())
            });

            let tmp_path = tokio::select! {
                biased;
                _ = abort_true(&mut abort) => {
                    anyhow::bail!("job aborted during git clone");
                }
                result = clone_task => {
                    result.context("fetch task panicked")??
                }
            };

            if let Some(spec) = &job.input_update {
                run_input_update(spec, &tmp_path, ssh_key.as_deref(), updater).await?;
            }

            let source_path = super::source::add_git_tree(store, &tmp_path, &commit).await?;
            (source_path, tmp_path)
        }
        FlakeSource::Cached { store_path } => {
            debug!(%store_path, has_ssh_key = ssh_key.is_some(), "archiving cached build source");
            (store_path.clone(), store_path.clone())
        }
    };

    let overrides_in: Vec<OverrideInput> = job.input_overrides.iter().map(Into::into).collect();
    let (applied_overrides, warnings) = if overrides_in.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let lock_path = std::path::Path::new(&flake_root).join("flake.lock");
        let lock_bytes = tokio::fs::read(&lock_path)
            .await
            .with_context(|| format!("failed to read {}", lock_path.display()))?;
        let lock: serde_json::Value =
            serde_json::from_slice(&lock_bytes).context("failed to parse flake.lock")?;
        let declared = declared_inputs_from_lock(&lock)?;
        resolve_overrides(&overrides_in, &declared, &lock)?
    };

    for msg in &warnings {
        updater
            .send_eval_message(
                gradient_wire::types::EvalMessageLevel::Warning,
                "fetch",
                msg,
            )
            .await?;
    }

    if !applied_overrides.is_empty() {
        info!(
            count = applied_overrides.len(),
            "applying flake input overrides"
        );
    }

    let tools = NixTools {
        nix: binpath_nix,
        ssh: binpath_ssh,
        store,
    };
    match archive_flake(
        &source_path,
        &flake_root,
        &tools,
        ssh_key.as_deref(),
        &applied_overrides,
        &mut abort,
    )
    .await
    {
        Ok(archived_paths) => {
            info!(%source_path, inputs = archived_paths.len(), "flake archived to nix store");
            Ok(FetchOutcome {
                source_path,
                archived_paths,
            })
        }
        // `nix flake archive` is all-or-nothing. One unfetchable private input is failing the
        // whole command, even when no eval target references it. Each locked input is
        // prefetched on its own instead, as best-effort cache population.
        Err(archive_err) => {
            let archive_msg = archive_err.to_string();
            warn!(error = %archive_msg, "nix flake archive failed; falling back to per-input prefetch");
            match prefetch_flake_best_effort(
                &source_path,
                &flake_root,
                &tools,
                ssh_key.as_deref(),
                &applied_overrides,
                &mut abort,
            )
            .await
            {
                Ok((archived_paths, input_warnings)) => {
                    updater
                        .send_eval_message(
                            gradient_wire::types::EvalMessageLevel::Warning,
                            "fetch",
                            &format!(
                                "nix flake archive failed, continued with best-effort per-input fetch: {}",
                                archive_msg.trim()
                            ),
                        )
                        .await?;
                    for msg in &input_warnings {
                        updater
                            .send_eval_message(
                                gradient_wire::types::EvalMessageLevel::Warning,
                                "fetch",
                                msg,
                            )
                            .await?;
                    }
                    info!(%source_path, inputs = archived_paths.len(), "flake prefetched to nix store after archive fallback");
                    Ok(FetchOutcome {
                        source_path,
                        archived_paths,
                    })
                }
                Err(prefetch_err) => Err(prefetch_err
                    .context(format!("nix flake archive failed: {}", archive_msg.trim()))),
            }
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
async fn run_input_update(
    spec: &gradient_wire::messages::InputUpdateSpec,
    checkout: &str,
    ssh_key: Option<&str>,
    updater: &mut dyn JobReporter,
) -> Result<()> {
    use gradient_sources::flake_lock::PatchGenerator as _;

    if spec.discover_only {
        let lock_path = std::path::Path::new(checkout).join("flake.lock");
        let bytes = tokio::fs::read(&lock_path)
            .await
            .with_context(|| format!("failed to read {}", lock_path.display()))?;
        let lock: serde_json::Value =
            serde_json::from_slice(&bytes).context("failed to parse flake.lock")?;
        let declared = declared_inputs_from_lock(&lock)?;
        let mut matched: Vec<String> = spec
            .inputs
            .iter()
            .filter(|p| gradient_util::glob::is_pattern(p))
            .flat_map(|p| {
                declared
                    .iter()
                    .filter(move |d| gradient_util::glob::glob_match(p, d))
                    .cloned()
            })
            .collect();
        matched.sort();
        matched.dedup();
        return updater.report_input_expansion(matched).await;
    }

    let resolver = gradient_sources::flake_lock::HttpRevisionResolver::new(
        gradient_worker_client::http::download_client().clone(),
    )
    .with_ssh_key(ssh_key.map(str::to_owned));
    let generator = gradient_sources::flake_lock::FlakeLockGenerator::new(resolver);
    let tracked: Vec<gradient_sources::flake_lock::InputName> =
        spec.inputs.iter().cloned().map(Into::into).collect();

    let Some(patch) = generator
        .produce(std::path::Path::new(checkout), &tracked)
        .await
        .context("flake.lock update generator failed")?
    else {
        return Ok(());
    };

    for edit in &patch.edits {
        let dest = std::path::Path::new(checkout).join(&edit.path);
        tokio::fs::write(&dest, &edit.contents)
            .await
            .with_context(|| format!("writing {}", dest.display()))?;
    }

    let candidate = patch
        .edits
        .iter()
        .find(|e| e.path.to_string_lossy().as_ref() == "flake.lock")
        .map(|e| String::from_utf8_lossy(&e.contents).into_owned())
        .unwrap_or_default();

    let bumped = patch
        .bumped
        .into_iter()
        .map(|b| gradient_wire::messages::BumpedInputWire {
            name: b.name,
            old_rev: b.old_rev,
            new_rev: b.new_rev,
        })
        .collect();

    updater.report_input_update(candidate, bumped).await
}

fn parse_nix_json(stdout: &[u8], cmd: &str) -> Result<serde_json::Value> {
    serde_json::from_slice(stdout).with_context(|| format!("failed to parse {cmd} JSON"))
}

fn build_archive_argv(flake_ref: &str, overrides: &[(String, String)]) -> Vec<String> {
    let mut argv = vec!["flake".to_owned(), "archive".to_owned()];
    for (name, ref_str) in overrides {
        argv.push("--override-input".to_owned());
        argv.push(name.clone());
        argv.push(ref_str.clone());
    }
    argv.push("--json".to_owned());
    argv.push(flake_ref.to_owned());
    argv
}

#[tracing::instrument(level = "debug", skip_all)]
async fn archive_flake(
    source_path: &str,
    flake_root: &str,
    tools: &NixTools<'_>,
    ssh_key: Option<&str>,
    overrides: &[(String, String)],
    abort: &mut watch::Receiver<bool>,
) -> Result<Vec<String>> {
    let NixTools {
        nix: binpath_nix,
        ssh: binpath_ssh,
        store,
    } = *tools;
    if overrides.is_empty()
        && let Some(mut inputs) = present_locked_inputs(flake_root, store).await
    {
        debug!(%source_path, inputs = inputs.len(), "every locked input present; nothing to fetch");
        inputs.push(source_path.to_owned());
        return Ok(inputs);
    }

    let flake_ref = format!("path:{source_path}");
    trace!(binpath_nix, flake_ref, "executing nix flake archive");
    let key_env = ssh_key_env(ssh_key, binpath_ssh).await?;
    let mut cmd = tokio::process::Command::new(binpath_nix);
    cmd.args(build_archive_argv(&flake_ref, overrides));
    if let Some((_guard, ssh_command)) = &key_env {
        cmd.env("GIT_SSH_COMMAND", ssh_command);
    }
    let output = run_nix_subprocess(cmd, "nix flake archive", abort).await?;

    let json: serde_json::Value = parse_nix_json(&output.stdout, "nix flake archive")?;
    let mut all_paths: HashSet<String> = HashSet::new();
    all_paths.insert(source_path.to_owned());
    collect_input_paths(&json, &mut all_paths);

    let all_paths: Vec<String> = all_paths.into_iter().collect();
    require_present(store, &all_paths).await?;

    Ok(all_paths)
}

async fn run_nix_subprocess(
    mut cmd: tokio::process::Command,
    label: &str,
    abort: &mut watch::Receiver<bool>,
) -> Result<std::process::Output> {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {label}"))?;

    #[expect(
        clippy::disallowed_methods,
        reason = "reaping a kill_on_drop child, aborted by the select below rather than by shutdown"
    )]
    let task = tokio::spawn(async move { child.wait_with_output().await });
    let abort_handle = task.abort_handle();

    let output = tokio::select! {
        biased;
        _ = abort_true(abort) => {
            abort_handle.abort();
            anyhow::bail!("job aborted during {label}");
        }
        result = task => {
            result
                .with_context(|| format!("{label} task panicked"))?
                .with_context(|| format!("failed to run {label}"))?
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{label} failed: {}", stderr.trim());
    }

    Ok(output)
}

async fn ssh_key_env(
    ssh_key: Option<&str>,
    binpath_ssh: &str,
) -> Result<Option<(NamedTempFile, String)>> {
    use std::os::unix::fs::PermissionsExt;

    let Some(key) = ssh_key else {
        return Ok(None);
    };
    let kf = NamedTempFile::with_suffix(".key").context("failed to create SSH key temp file")?;
    tokio::fs::set_permissions(kf.path(), std::fs::Permissions::from_mode(0o600))
        .await
        .context("failed to chmod SSH key file")?;
    tokio::fs::write(kf.path(), key.as_bytes())
        .await
        .context("failed to write SSH key file")?;
    let ssh_command = format!(
        "{} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
        binpath_ssh,
        kf.path().display()
    );
    Ok(Some((kf, ssh_command)))
}

fn build_prefetch_argv(flake_ref: &str) -> Vec<String> {
    let mut argv = vec!["flake".to_owned(), "prefetch".to_owned()];
    if !is_indirect(flake_ref) {
        argv.extend([
            "--option".to_owned(),
            "flake-registry".to_owned(),
            String::new(),
        ]);
    }
    argv.extend(["--json".to_owned(), flake_ref.to_owned()]);
    argv
}

fn is_indirect(flake_ref: &str) -> bool {
    flake_ref.starts_with("flake:") || !flake_ref.contains(':')
}

#[tracing::instrument(level = "debug", skip_all)]
async fn prefetch_one(
    flake_ref: &str,
    binpath_nix: &str,
    ssh_command: Option<&str>,
    abort: &mut watch::Receiver<bool>,
) -> Result<String> {
    trace!(binpath_nix, flake_ref, "executing nix flake prefetch");
    let mut cmd = tokio::process::Command::new(binpath_nix);
    cmd.args(build_prefetch_argv(flake_ref));
    if let Some(sc) = ssh_command {
        cmd.env("GIT_SSH_COMMAND", sc);
    }
    let output = run_nix_subprocess(cmd, "nix flake prefetch", abort).await?;
    let json = parse_nix_json(&output.stdout, "nix flake prefetch")?;
    json["storePath"]
        .as_str()
        .context("nix flake prefetch JSON missing 'storePath' field")
        .map(str::to_owned)
}

#[tracing::instrument(level = "debug", skip_all)]
async fn prefetch_flake_best_effort(
    source_path: &str,
    flake_root: &str,
    tools: &NixTools<'_>,
    ssh_key: Option<&str>,
    overrides: &[(String, String)],
    abort: &mut watch::Receiver<bool>,
) -> Result<(Vec<String>, Vec<String>)> {
    let NixTools {
        nix: binpath_nix,
        ssh: binpath_ssh,
        store,
    } = *tools;
    let key_env = ssh_key_env(ssh_key, binpath_ssh).await?;
    let ssh_command = key_env.as_ref().map(|(_, c)| c.as_str());

    let mut all_paths: HashSet<String> = HashSet::new();
    all_paths.insert(source_path.to_owned());
    let mut warnings: Vec<String> = Vec::new();

    let lock_path = std::path::Path::new(flake_root).join("flake.lock");
    match tokio::fs::read(&lock_path).await {
        Ok(bytes) => {
            let lock: serde_json::Value =
                serde_json::from_slice(&bytes).context("failed to parse flake.lock")?;
            let (refs, walk_warnings) = prefetch_refs_from_lock(&lock, overrides);
            warnings.extend(walk_warnings);
            for (name, input_ref) in refs {
                if *abort.borrow() {
                    anyhow::bail!("job aborted during flake input prefetch");
                }
                match prefetch_one(&input_ref, binpath_nix, ssh_command, abort).await {
                    Ok(path) => {
                        all_paths.insert(path);
                    }
                    Err(e) => warnings.push(format!(
                        "skipping flake input '{name}': {}",
                        e.to_string().trim()
                    )),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", lock_path.display()));
        }
    }

    let all_paths: Vec<String> = all_paths.into_iter().collect();
    require_present(store, &all_paths).await?;

    Ok((all_paths, warnings))
}

fn collect_input_paths(node: &serde_json::Value, paths: &mut HashSet<String>) {
    if let Some(inputs) = node["inputs"].as_object() {
        for input in inputs.values() {
            if let Some(path) = input["path"].as_str() {
                paths.insert(path.to_owned());
            }
            collect_input_paths(input, paths);
        }
    }
}

async fn require_present(store: &dyn WorkerStore, paths: &[String]) -> Result<()> {
    let missing = missing_paths(store, paths).await?;
    if !missing.is_empty() {
        anyhow::bail!("not in the local store: {}", missing.join(", "));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all, fields(paths = paths.len()))]
async fn missing_paths(store: &dyn WorkerStore, paths: &[String]) -> Result<Vec<String>> {
    let valid =
        futures::future::try_join_all(paths.iter().map(|path| store.has_path(path))).await?;
    Ok(paths
        .iter()
        .zip(valid)
        .filter(|(_, valid)| !valid)
        .map(|(path, _)| path.clone())
        .collect())
}

async fn present_locked_inputs(flake_root: &str, store: &dyn WorkerStore) -> Option<Vec<String>> {
    let bytes = tokio::fs::read(std::path::Path::new(flake_root).join("flake.lock"))
        .await
        .ok()?;
    let lock: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let paths = locked_input_paths(&lock)?;
    match missing_paths(store, &paths).await {
        Ok(missing) if missing.is_empty() => Some(paths),
        Ok(missing) => {
            debug!(?missing, "a locked input is not in the store; archiving");
            None
        }
        Err(e) => {
            debug!(error = %e, "store presence unknown; archiving");
            None
        }
    }
}

fn locked_input_paths(lock: &serde_json::Value) -> Option<Vec<String>> {
    use harmonia_store_content_address::{ContentAddress, make_store_path_from_ca};
    use harmonia_store_path::{StoreDir, StorePathName};
    use harmonia_utils_hash::{Hash, fmt::SRI};

    let root = lock.get("root")?.as_str()?;
    let store_dir = StoreDir::default();
    let name: StorePathName = "source".parse().ok()?;
    let mut paths: Vec<String> = lock
        .get("nodes")?
        .as_object()?
        .iter()
        .filter(|(key, _)| key.as_str() != root)
        .map(|(_, node)| -> Option<String> {
            let nar_hash = node.get("locked")?.get("narHash")?.as_str()?;
            let hash = nar_hash.parse::<SRI<Hash>>().ok()?.into_hash();
            let path =
                make_store_path_from_ca(&store_dir, name.clone(), ContentAddress::NixArchive(hash));
            Some(store_dir.display(&path).to_string())
        })
        .collect::<Option<_>>()?;
    paths.sort_unstable();
    paths.dedup();
    Some(paths)
}

#[tracing::instrument(level = "debug", skip_all)]
fn clone_and_checkout(url: &str, commit: &str, ssh_key: Option<&str>) -> Result<String> {
    let temp_dir = std::env::temp_dir().join(format!("gradient-fetch-{}", uuid::Uuid::now_v7()));

    let repo = git2::build::RepoBuilder::new()
        .fetch_options(gradient_sources::fetch_options_with_ssh(ssh_key))
        .clone(url, &temp_dir)
        .with_context(|| format!("failed to clone {url}"))?;

    let oid =
        git2::Oid::from_str(commit).with_context(|| format!("invalid commit SHA: {commit}"))?;

    let git_commit = match repo.find_commit(oid) {
        Ok(c) => c,
        Err(_) => {
            repo.find_remote("origin")
                .context("failed to find origin remote")?
                .fetch(
                    &[commit],
                    Some(&mut gradient_sources::fetch_options_with_ssh(ssh_key)),
                    None,
                )
                .with_context(|| {
                    format!(
                        "commit {commit} not reachable in {url} (force-pushed, GC'd, or a fork PR ref)"
                    )
                })?;

            repo.find_commit(oid).with_context(|| {
                format!("commit {commit} still not found in {url} after fetching it directly")
            })?
        }
    };

    let tree = git_commit.tree().context("failed to get commit tree")?;

    let mut co = git2::build::CheckoutBuilder::new();
    co.force();

    repo.checkout_tree(tree.as_object(), Some(&mut co))
        .context("checkout failed")?;

    // HEAD is staying on the default branch from the clone. Nix is reading files at the pinned
    // `?rev=` and is warning "could not read HEAD ref" on a detached HEAD.
    info!(path = %temp_dir.display(), %commit, "repository cloned");
    Ok(temp_dir.to_string_lossy().into_owned())
}

fn flake_ref_from_lock_original(original: &serde_json::Value) -> anyhow::Result<String> {
    use anyhow::Context;
    let ty = original
        .get("type")
        .and_then(|v| v.as_str())
        .context("flake.lock node.original missing 'type'")?;

    let str_field = |k: &str| -> Option<&str> { original.get(k).and_then(|v| v.as_str()) };

    Ok(match ty {
        "github" | "gitlab" | "sourcehut" => {
            let owner = str_field("owner").with_context(|| format!("{ty} node missing 'owner'"))?;
            let repo = str_field("repo").with_context(|| format!("{ty} node missing 'repo'"))?;
            match str_field("ref") {
                Some(r) => format!("{ty}:{owner}/{repo}/{r}"),
                None => format!("{ty}:{owner}/{repo}"),
            }
        }
        "git" => {
            let url = str_field("url").context("git node missing 'url'")?;
            format!("git+{url}")
        }
        "tarball" => {
            let url = str_field("url").context("tarball node missing 'url'")?;
            url.to_owned()
        }
        "path" => {
            let path = str_field("path").context("path node missing 'path'")?;
            format!("path:{path}")
        }
        "indirect" => {
            let id = str_field("id").context("indirect node missing 'id'")?;
            format!("flake:{id}")
        }
        other => anyhow::bail!("unsupported flake.lock input type '{other}'"),
    })
}

fn percent_encode_hash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '+' => out.push_str("%2B"),
            '/' => out.push_str("%2F"),
            '=' => out.push_str("%3D"),
            other => out.push(other),
        }
    }
    out
}

fn nar_hash_param(locked: &serde_json::Value, sep: char) -> String {
    match locked.get("narHash").and_then(|v| v.as_str()) {
        Some(h) => format!("{sep}narHash={}", percent_encode_hash(h)),
        None => String::new(),
    }
}

fn flake_ref_from_lock_locked(locked: &serde_json::Value) -> anyhow::Result<String> {
    use anyhow::Context;
    let ty = locked
        .get("type")
        .and_then(|v| v.as_str())
        .context("flake.lock node.locked missing 'type'")?;

    let s = |k: &str| -> Option<&str> { locked.get(k).and_then(|v| v.as_str()) };

    Ok(match ty {
        "github" | "gitlab" | "sourcehut" => {
            let owner = s("owner").with_context(|| format!("{ty} node missing 'owner'"))?;
            let repo = s("repo").with_context(|| format!("{ty} node missing 'repo'"))?;
            let rev = s("rev").with_context(|| format!("{ty} node missing 'rev'"))?;
            format!("{ty}:{owner}/{repo}/{rev}{}", nar_hash_param(locked, '?'))
        }
        "git" => {
            let url = s("url").context("git node missing 'url'")?;
            let mut params: Vec<String> = Vec::new();
            if let Some(r) = s("ref") {
                params.push(format!("ref={r}"));
            }
            if let Some(rev) = s("rev") {
                params.push(format!("rev={rev}"));
            }
            if let Some(h) = s("narHash") {
                params.push(format!("narHash={}", percent_encode_hash(h)));
            }
            if params.is_empty() {
                format!("git+{url}")
            } else {
                let sep = if url.contains('?') { '&' } else { '?' };
                format!("git+{url}{sep}{}", params.join("&"))
            }
        }
        "tarball" => {
            let url = s("url").context("tarball node missing 'url'")?;
            let sep = if url.contains('?') { '&' } else { '?' };
            format!("{url}{}", nar_hash_param(locked, sep))
        }
        "path" => {
            let path = s("path").context("path node missing 'path'")?;
            format!("path:{path}")
        }
        other => anyhow::bail!("unsupported flake.lock locked type '{other}'"),
    })
}

fn prefetch_refs_from_lock(
    lock: &serde_json::Value,
    overrides: &[(String, String)],
) -> (Vec<(String, String)>, Vec<String>) {
    let root_key = lock.get("root").and_then(|v| v.as_str()).unwrap_or("root");
    let override_map: std::collections::HashMap<&str, &str> = overrides
        .iter()
        .map(|(n, r)| (n.as_str(), r.as_str()))
        .collect();

    let root_input_of: std::collections::HashMap<&str, &str> = lock
        .get("nodes")
        .and_then(|n| n.get(root_key))
        .and_then(|r| r.get("inputs"))
        .and_then(|i| i.as_object())
        .map(|inputs| {
            inputs
                .iter()
                .filter_map(|(name, key)| key.as_str().map(|k| (k, name.as_str())))
                .collect()
        })
        .unwrap_or_default();

    let Some(nodes) = lock.get("nodes").and_then(|n| n.as_object()) else {
        return (Vec::new(), Vec::new());
    };

    let mut refs = Vec::new();
    let mut warnings = Vec::new();
    for (node_key, node) in nodes {
        if node_key == root_key {
            continue;
        }
        let root_name = root_input_of.get(node_key.as_str()).copied();
        let display_name = root_name.unwrap_or(node_key.as_str());

        if let Some(override_ref) = root_name.and_then(|name| override_map.get(name)) {
            refs.push((display_name.to_owned(), (*override_ref).to_owned()));
            continue;
        }

        let Some(locked) = node.get("locked") else {
            continue;
        };
        match flake_ref_from_lock_locked(locked) {
            Ok(r) => refs.push((display_name.to_owned(), r)),
            Err(e) => warnings.push(format!("skipping flake input '{display_name}': {e}")),
        }
    }
    (refs, warnings)
}

#[derive(Debug, Clone)]
pub struct OverrideInput {
    pub input_name: String,
    pub url: Option<String>,
}

impl From<&gradient_wire::types::FlakeInputOverride> for OverrideInput {
    fn from(o: &gradient_wire::types::FlakeInputOverride) -> Self {
        Self {
            input_name: o.input_name.clone(),
            url: o.url.clone(),
        }
    }
}

type AppliedOverride = (String, String);

fn resolve_overrides(
    overrides: &[OverrideInput],
    declared: &std::collections::HashSet<String>,
    lock: &serde_json::Value,
) -> anyhow::Result<(Vec<AppliedOverride>, Vec<String>)> {
    let raw: Vec<(String, Option<String>)> = overrides
        .iter()
        .map(|o| (o.input_name.clone(), o.url.clone()))
        .collect();
    let declared_sorted: std::collections::BTreeSet<String> = declared.iter().cloned().collect();
    let (resolved, warnings) = gradient_util::glob::expand_overrides(&raw, &declared_sorted);

    let mut applied = Vec::with_capacity(resolved.len());
    for (input_name, url) in resolved {
        let ref_str = match url {
            Some(u) => u,
            None => reconstruct_original_ref(lock, &input_name)?,
        };
        applied.push((input_name, ref_str));
    }
    Ok((applied, warnings))
}

fn reconstruct_original_ref(lock: &serde_json::Value, input_name: &str) -> anyhow::Result<String> {
    let root_key = lock.get("root").and_then(|v| v.as_str()).unwrap_or("root");
    let node_key = lock
        .get("nodes")
        .and_then(|n| n.get(root_key))
        .and_then(|r| r.get("inputs"))
        .and_then(|i| i.get(input_name))
        .and_then(|k| k.as_str())
        .with_context(|| format!("flake.lock missing nodes.root.inputs.{input_name}"))?;
    let original = lock
        .get("nodes")
        .and_then(|n| n.get(node_key))
        .and_then(|n| n.get("original"))
        .with_context(|| format!("flake.lock missing nodes.{node_key}.original"))?;
    flake_ref_from_lock_original(original)
}

fn declared_inputs_from_lock(
    lock: &serde_json::Value,
) -> anyhow::Result<std::collections::HashSet<String>> {
    use anyhow::Context;
    let root_key = lock.get("root").and_then(|v| v.as_str()).unwrap_or("root");
    let root = lock
        .get("nodes")
        .and_then(|n| n.get(root_key))
        .with_context(|| format!("flake.lock missing nodes.{root_key}"))?;
    let Some(inputs) = root.get("inputs").and_then(|v| v.as_object()) else {
        return Ok(std::collections::HashSet::new());
    };
    Ok(inputs.keys().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::fakes::job_reporter::{RecordingJobReporter, ReportedEvent};
    use gradient_test_support::fakes::worker_store::FakeWorkerStore;
    use gradient_wire::messages::FlakeStep;

    fn make_flake_job() -> FlakeJob {
        FlakeJob {
            steps: vec![FlakeStep::FetchFlake],
            source: FlakeSource::Repository {
                url: "https://example.com/repo.git".into(),
                commit: "abc123".into(),
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        }
    }

    fn no_abort() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    #[tokio::test]
    async fn fetch_reports_fetching_and_succeeds() {
        let job = make_flake_job();
        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();

        let result = fetch_repository(
            &job,
            &mut reporter,
            &credentials,
            &FakeWorkerStore::new(),
            "nix",
            "ssh",
            no_abort(),
        )
        .await;

        assert_eq!(reporter.len(), 1);
        assert!(matches!(reporter.events()[0], ReportedEvent::Fetching));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_cached_source_does_not_bail_on_kind() {
        let job = FlakeJob {
            steps: vec![FlakeStep::FetchFlake],
            source: FlakeSource::Cached {
                store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source".into(),
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        };
        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();
        let result = fetch_repository(
            &job,
            &mut reporter,
            &credentials,
            &FakeWorkerStore::new(),
            "nix",
            "ssh",
            no_abort(),
        )
        .await;
        let msg = format!("{:?}", result.err());
        assert!(
            !msg.contains("requires FlakeSource::Repository"),
            "cached must be handled: {msg}"
        );
    }

    #[tokio::test]
    async fn fetch_repository_adds_the_clone_to_the_store_without_nix() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");

        let rd = repo_dir.to_str().unwrap();
        Command::new("git")
            .args(["init", rd, "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(repo_dir.join("flake.nix"), "{}").unwrap();
        Command::new("git")
            .args(["-C", rd, "add", "."])
            .output()
            .unwrap();
        let commit_out = Command::new("git")
            .args([
                "-C",
                rd,
                "-c",
                "user.name=test",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ])
            .output()
            .unwrap();
        assert!(
            commit_out.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit_out.stderr)
        );

        let sha = String::from_utf8(
            Command::new("git")
                .args(["-C", rd, "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert!(sha.len() == 40, "expected 40-char SHA, got: {sha}");

        let job = FlakeJob {
            steps: vec![FlakeStep::FetchFlake],
            source: FlakeSource::Repository {
                url: format!("file://{}", repo_dir.display()),
                commit: sha,
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        };

        let credentials = crate::proto::credentials::CredentialStore::new();
        let mut reporter = RecordingJobReporter::new();
        let store = FakeWorkerStore::new();

        let outcome = fetch_repository(
            &job,
            &mut reporter,
            &credentials,
            &store,
            "nix",
            "ssh",
            no_abort(),
        )
        .await
        .unwrap();

        assert!(outcome.source_path.starts_with("/nix/store/"));
        assert!(outcome.source_path.ends_with("-source"));
        assert_eq!(outcome.archived_paths, vec![outcome.source_path.clone()]);
        assert!(store.has_path(&outcome.source_path).await.unwrap());
        assert!(matches!(reporter.events()[0], ReportedEvent::Fetching));
    }

    #[test]
    fn a_locked_input_path_follows_from_its_nar_hash() {
        let lock = serde_json::json!({
            "nodes": {
                "crane": {"locked": {"narHash": "sha256-jtT4yxZpR8seYnlCMMWSSPlFN92zO6ICuZ8pJrmi86k=", "type": "github"}},
                "root": {"inputs": {"crane": "crane"}}
            },
            "root": "root",
            "version": 7
        });
        assert_eq!(
            locked_input_paths(&lock),
            Some(vec![
                "/nix/store/7rradzysxg41b2yx7qnh2f2bw73py192-source".to_owned()
            ])
        );
    }

    #[tokio::test]
    async fn missing_paths_are_the_ones_the_store_does_not_hold() {
        let store = FakeWorkerStore::new().with_present_path("/nix/store/a-source");
        let paths = vec![
            "/nix/store/a-source".to_owned(),
            "/nix/store/b-source".to_owned(),
        ];
        assert_eq!(
            missing_paths(&store, &paths).await.unwrap(),
            vec!["/nix/store/b-source".to_owned()]
        );
    }

    #[test]
    fn a_direct_prefetch_never_loads_the_global_registry() {
        assert_eq!(
            build_prefetch_argv("git+file:///tmp/checkout?rev=abc"),
            [
                "flake",
                "prefetch",
                "--option",
                "flake-registry",
                "",
                "--json",
                "git+file:///tmp/checkout?rev=abc"
            ]
        );
        for indirect in ["nixpkgs", "nixpkgs/nixos-24.05", "flake:nixpkgs"] {
            assert_eq!(
                build_prefetch_argv(indirect),
                ["flake", "prefetch", "--json", indirect],
                "{indirect}"
            );
        }
    }

    #[test]
    fn an_input_without_a_nar_hash_sends_the_fetch_through_the_archive() {
        let lock = serde_json::json!({
            "nodes": {
                "sub": {"locked": {"path": "./sub", "type": "path"}},
                "root": {"inputs": {"sub": "sub"}}
            },
            "root": "root"
        });
        assert_eq!(locked_input_paths(&lock), None);
    }

    #[test]
    fn flake_ref_from_lock_original_github() {
        let original: serde_json::Value = serde_json::json!({
            "type": "github",
            "owner": "NixOS",
            "repo": "nixpkgs",
            "ref": "nixos-unstable",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "github:NixOS/nixpkgs/nixos-unstable",
        );
    }

    #[test]
    fn flake_ref_from_lock_original_github_no_ref() {
        let original: serde_json::Value = serde_json::json!({
            "type": "github",
            "owner": "NixOS",
            "repo": "nixpkgs",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "github:NixOS/nixpkgs",
        );
    }

    #[test]
    fn flake_ref_from_lock_original_indirect() {
        let original: serde_json::Value = serde_json::json!({
            "type": "indirect",
            "id": "flake-utils",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "flake:flake-utils",
        );
    }

    #[test]
    fn flake_ref_from_lock_original_git_url() {
        let original: serde_json::Value = serde_json::json!({
            "type": "git",
            "url": "https://example.test/r.git",
        });
        assert_eq!(
            super::flake_ref_from_lock_original(&original).unwrap(),
            "git+https://example.test/r.git",
        );
    }

    #[test]
    fn build_archive_argv_appends_override_input_flags() {
        let overrides = [
            (
                "nixpkgs".to_owned(),
                "github:NixOS/nixpkgs/nixos-unstable".to_owned(),
            ),
            ("utils".to_owned(), "flake:flake-utils".to_owned()),
        ];
        let argv = super::build_archive_argv("git+file:///tmp/x?rev=abc", &overrides);
        assert_eq!(
            argv,
            vec![
                "flake".to_owned(),
                "archive".to_owned(),
                "--override-input".to_owned(),
                "nixpkgs".to_owned(),
                "github:NixOS/nixpkgs/nixos-unstable".to_owned(),
                "--override-input".to_owned(),
                "utils".to_owned(),
                "flake:flake-utils".to_owned(),
                "--json".to_owned(),
                "git+file:///tmp/x?rev=abc".to_owned(),
            ],
        );
    }

    #[test]
    fn declared_inputs_from_lock_reads_root_inputs() {
        let lock: serde_json::Value = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "nixpkgs": "nixpkgs", "flake-utils": "flake-utils" } },
                "nixpkgs": { "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" } },
                "flake-utils": { "original": { "type": "indirect", "id": "flake-utils" } },
            },
            "root": "root",
        });
        let names = super::declared_inputs_from_lock(&lock).unwrap();
        assert!(names.contains("nixpkgs"));
        assert!(names.contains("flake-utils"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn resolve_overrides_keeps_url_some() {
        let declared: std::collections::HashSet<String> =
            ["nixpkgs".to_owned()].into_iter().collect();
        let lock = serde_json::json!({"nodes":{"root":{"inputs":{"nixpkgs":"nixpkgs"}}}});
        let overrides = [super::OverrideInput {
            input_name: "nixpkgs".into(),
            url: Some("github:NixOS/nixpkgs/nixos-unstable".into()),
        }];
        let (applied, warnings) = super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        assert_eq!(
            applied,
            vec![(
                "nixpkgs".to_owned(),
                "github:NixOS/nixpkgs/nixos-unstable".to_owned()
            )]
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_overrides_keep_url_reconstructs_from_lock() {
        let declared: std::collections::HashSet<String> =
            ["nixpkgs".to_owned()].into_iter().collect();
        let lock = serde_json::json!({
            "nodes": {
                "root": {"inputs": {"nixpkgs": "nixpkgs"}},
                "nixpkgs": {"original": {"type":"github","owner":"NixOS","repo":"nixpkgs","ref":"nixos-unstable"}},
            },
            "root": "root",
        });
        let overrides = [super::OverrideInput {
            input_name: "nixpkgs".into(),
            url: None,
        }];
        let (applied, warnings) = super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        assert_eq!(
            applied,
            vec![(
                "nixpkgs".to_owned(),
                "github:NixOS/nixpkgs/nixos-unstable".to_owned()
            )]
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_overrides_expands_glob() {
        let declared: std::collections::HashSet<String> = ["nixpkgs", "nixpkgs-lib", "flake-utils"]
            .into_iter()
            .map(String::from)
            .collect();
        let lock = serde_json::json!({
            "nodes": {
                "root": {"inputs": {"nixpkgs": "nixpkgs", "nixpkgs-lib": "nixpkgs-lib", "flake-utils": "flake-utils"}},
                "nixpkgs": {"original": {"type":"github","owner":"NixOS","repo":"nixpkgs","ref":"nixos-unstable"}},
                "nixpkgs-lib": {"original": {"type":"github","owner":"nix-community","repo":"nixpkgs.lib"}},
                "flake-utils": {"original": {"type":"github","owner":"numtide","repo":"flake-utils"}},
            },
            "root": "root",
        });
        let overrides = [super::OverrideInput {
            input_name: "nixpkgs*".into(),
            url: None,
        }];
        let (applied, _warnings) = super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        let names: std::collections::BTreeSet<&str> =
            applied.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains("nixpkgs"));
        assert!(names.contains("nixpkgs-lib"));
        assert!(!names.contains("flake-utils"));
    }

    #[test]
    fn resolve_overrides_unknown_input_drops_with_warning() {
        let declared: std::collections::HashSet<String> =
            ["nixpkgs".to_owned()].into_iter().collect();
        let lock = serde_json::json!({"nodes":{"root":{"inputs":{"nixpkgs":"nixpkgs"}}}});
        let overrides = [super::OverrideInput {
            input_name: "missing".into(),
            url: Some("github:x/y".into()),
        }];
        let (applied, warnings) = super::resolve_overrides(&overrides, &declared, &lock).unwrap();
        assert!(applied.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing"));
    }

    #[test]
    fn percent_encode_hash_encodes_base64_specials() {
        assert_eq!(
            super::percent_encode_hash("sha256:A+b/C="),
            "sha256:A%2Bb%2FC%3D"
        );
        assert_eq!(
            super::percent_encode_hash("plain-hash_123"),
            "plain-hash_123"
        );
    }

    #[test]
    fn flake_ref_from_lock_locked_github_rev_narhash() {
        let locked = serde_json::json!({
            "type": "github",
            "owner": "NixOS",
            "repo": "nixpkgs",
            "rev": "deadbeef",
            "narHash": "sha256:x+y/z=",
        });
        assert_eq!(
            super::flake_ref_from_lock_locked(&locked).unwrap(),
            "github:NixOS/nixpkgs/deadbeef?narHash=sha256:x%2By%2Fz%3D",
        );
    }

    #[test]
    fn flake_ref_from_lock_locked_git_ref_rev() {
        let locked = serde_json::json!({
            "type": "git",
            "url": "ssh://git@example.test/r.git",
            "ref": "main",
            "rev": "abc123",
        });
        assert_eq!(
            super::flake_ref_from_lock_locked(&locked).unwrap(),
            "git+ssh://git@example.test/r.git?ref=main&rev=abc123",
        );
    }

    #[test]
    fn flake_ref_from_lock_locked_tarball_narhash() {
        let locked = serde_json::json!({
            "type": "tarball",
            "url": "https://example.test/x.tar.gz",
            "narHash": "sha256:a/b=",
        });
        assert_eq!(
            super::flake_ref_from_lock_locked(&locked).unwrap(),
            "https://example.test/x.tar.gz?narHash=sha256:a%2Fb%3D",
        );
    }

    #[test]
    fn flake_ref_from_lock_locked_path() {
        let locked = serde_json::json!({
            "type": "path",
            "path": "/nix/store/xxx-source",
        });
        assert_eq!(
            super::flake_ref_from_lock_locked(&locked).unwrap(),
            "path:/nix/store/xxx-source",
        );
    }

    #[test]
    fn prefetch_refs_from_lock_walks_all_and_applies_override() {
        let lock = serde_json::json!({
            "root": "root",
            "nodes": {
                "root": { "inputs": { "nixpkgs": "nixpkgs", "secret": "secret" } },
                "nixpkgs": {
                    "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "aaa" },
                    "inputs": { "flake-utils": "flake-utils" }
                },
                "flake-utils": {
                    "locked": { "type": "github", "owner": "numtide", "repo": "flake-utils", "rev": "bbb" }
                },
                "secret": {
                    "locked": { "type": "git", "url": "ssh://git@host/secret.git", "rev": "ccc" }
                },
                "weird": {
                    "locked": { "type": "mercurial", "url": "http://x" }
                }
            }
        });
        let overrides = [(
            "nixpkgs".to_owned(),
            "github:NixOS/nixpkgs/override-rev".to_owned(),
        )];
        let (refs, warnings) = super::prefetch_refs_from_lock(&lock, &overrides);
        let map: std::collections::HashMap<String, String> = refs.into_iter().collect();
        assert_eq!(
            map.get("nixpkgs").map(String::as_str),
            Some("github:NixOS/nixpkgs/override-rev")
        );
        assert_eq!(
            map.get("flake-utils").map(String::as_str),
            Some("github:numtide/flake-utils/bbb")
        );
        assert_eq!(
            map.get("secret").map(String::as_str),
            Some("git+ssh://git@host/secret.git?rev=ccc")
        );
        assert!(!map.contains_key("root"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("weird"));
    }
}
