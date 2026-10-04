/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use gradient_wire::messages::{FlakeJob, FlakeSource};
use gradient_wire::traits::{EvalProgressSink, JobReporter, WorkerStore};
use gradient_wire::types::{EvalProgress, InputFetchState};
use tempfile::NamedTempFile;
use tracing::{debug, info};

use super::AbortSignal;
use super::progress_report::ChangeReporter;
use crate::proto::credentials::CredentialStore;
use crate::worker_pool::{DownloadTarget, InputBoard, InputFetcher};

pub struct FetchOutcome {
    pub source_path: String,
    pub input_paths: Vec<String>,
    pub progress: Option<EvalProgress>,
}

#[derive(Debug)]
struct FetchedInputs {
    paths: Vec<String>,
    warnings: Vec<String>,
    progress: Option<EvalProgress>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &CredentialStore,
    store: &dyn WorkerStore,
    fetcher: &dyn InputFetcher,
    binpath_ssh: &str,
    mut abort: AbortSignal,
) -> Result<FetchOutcome> {
    if abort.is_aborted() {
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
                () = abort.aborted() => {
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
            debug!(%store_path, has_ssh_key = ssh_key.is_some(), "using the cached build source");
            (store_path.clone(), store_path.clone())
        }
    };

    let lock = read_lock(&flake_root).await?;
    let overrides_in: Vec<OverrideInput> = job.input_overrides.iter().map(Into::into).collect();
    let (applied_overrides, warnings) = match (&lock, overrides_in.is_empty()) {
        (_, true) => (Vec::new(), Vec::new()),
        (Some(lock), false) => {
            let declared = declared_inputs_from_lock(lock)?;
            resolve_overrides(&overrides_in, &declared, lock)?
        }
        (None, false) => anyhow::bail!("input overrides need a flake.lock in {flake_root}"),
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

    let overridden: HashSet<String> = applied_overrides
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let inputs = lock
        .as_ref()
        .map(|lock| locked_inputs(lock, &overridden))
        .unwrap_or_default();
    let key_env = ssh_key_env(ssh_key.as_deref(), binpath_ssh).await?;
    let git_ssh_command = key_env.as_ref().map(|(_, command)| command.clone());
    let sink = updater.eval_progress_sink();
    let fetched = fetch_inputs(inputs, store, fetcher, &*sink, git_ssh_command, &mut abort).await?;
    for msg in &fetched.warnings {
        updater
            .send_eval_message(
                gradient_wire::types::EvalMessageLevel::Warning,
                "fetch",
                msg,
            )
            .await?;
    }
    let mut input_paths = fetched.paths;
    input_paths.push(source_path.clone());
    require_present(store, &input_paths).await?;
    info!(%source_path, inputs = input_paths.len(), "flake inputs in the nix store");
    Ok(FetchOutcome {
        source_path,
        input_paths,
        progress: fetched.progress,
    })
}

#[derive(Debug, Clone)]
struct LockedInput {
    name: String,
    domain: String,
    store_path: String,
    locked: String,
}

async fn read_lock(flake_root: &str) -> Result<Option<serde_json::Value>> {
    let path = std::path::Path::new(flake_root).join("flake.lock");
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("failed to parse flake.lock")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn locked_inputs(lock: &serde_json::Value, overridden: &HashSet<String>) -> Vec<LockedInput> {
    let root = lock["root"].as_str().unwrap_or("root");
    let nodes = &lock["nodes"];
    let root_names: HashMap<&str, &str> = nodes[root]["inputs"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, key)| Some((key.as_str()?, name.as_str())))
        .collect();

    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: Vec<&str> = root_names
        .iter()
        .filter(|(_, name)| !overridden.contains(**name))
        .map(|(key, _)| *key)
        .collect();
    let mut inputs = Vec::new();
    while let Some(key) = queue.pop() {
        if !seen.insert(key) {
            continue;
        }
        let node = &nodes[key];
        queue.extend(
            node["inputs"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, k)| k.as_str()),
        );
        let locked = &node["locked"];
        let Some(store_path) = locked["narHash"].as_str().and_then(source_store_path) else {
            continue;
        };
        inputs.push(LockedInput {
            name: root_names.get(key).copied().unwrap_or(key).to_owned(),
            domain: input_domain(locked),
            store_path,
            locked: fetcher_attrs(locked).to_string(),
        });
    }
    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    inputs
}

fn fetcher_attrs(locked: &serde_json::Value) -> serde_json::Value {
    let mut attrs = locked.clone();
    if let Some(attrs) = attrs.as_object_mut() {
        attrs.remove("dir");
    }
    attrs
}

fn source_store_path(nar_hash: &str) -> Option<String> {
    use harmonia_store_content_address::{ContentAddress, make_store_path_from_ca};
    use harmonia_store_path::{StoreDir, StorePathName};
    use harmonia_utils_hash::{Hash, fmt::SRI};

    let store_dir = StoreDir::default();
    let name: StorePathName = "source".parse().ok()?;
    let hash = nar_hash.parse::<SRI<Hash>>().ok()?.into_hash();
    let path = make_store_path_from_ca(&store_dir, name, ContentAddress::NixArchive(hash));
    Some(store_dir.display(&path).to_string())
}

fn input_domain(locked: &serde_json::Value) -> String {
    let default_host = match locked["type"].as_str() {
        Some("github") => Some("github.com"),
        Some("gitlab") => Some("gitlab.com"),
        Some("sourcehut") => Some("git.sr.ht"),
        _ => None,
    };
    let host = match default_host {
        Some(default) => locked["host"].as_str().unwrap_or(default),
        None => locked["url"].as_str().map(url_host).unwrap_or_default(),
    };
    second_level(host)
}

fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.split(':').next().unwrap_or_default()
}

fn second_level(host: &str) -> String {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_owned();
    }
    let labels: Vec<&str> = host.rsplitn(3, '.').collect();
    match labels.as_slice() {
        [tld, sld, ..] => format!("{sld}.{tld}"),
        _ => host.to_owned(),
    }
}

#[tracing::instrument(level = "debug", skip_all, fields(inputs = inputs.len()))]
async fn fetch_inputs(
    inputs: Vec<LockedInput>,
    store: &dyn WorkerStore,
    fetcher: &dyn InputFetcher,
    sink: &dyn EvalProgressSink,
    git_ssh_command: Option<String>,
    abort: &mut AbortSignal,
) -> Result<FetchedInputs> {
    let paths: Vec<String> = inputs.iter().map(|i| i.store_path.clone()).collect();
    let missing: HashSet<String> = missing_paths(store, &paths).await?.into_iter().collect();
    let board = InputBoard::new(
        inputs
            .iter()
            .map(|i| {
                let state = if missing.contains(&i.store_path) {
                    InputFetchState::Queued
                } else {
                    InputFetchState::Done
                };
                (i.name.clone(), state)
            })
            .collect(),
    );

    let mut by_domain: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, input) in inputs
        .iter()
        .enumerate()
        .filter(|(_, i)| missing.contains(&i.store_path))
    {
        by_domain
            .entry(input.domain.as_str())
            .or_default()
            .push(index);
    }
    let fetch_domain = |indexes: Vec<usize>| {
        let (board, inputs, git_ssh_command) = (&board, &inputs, &git_ssh_command);
        async move {
            let mut done = Vec::new();
            for index in indexes {
                board.set_state(index, InputFetchState::Fetching);
                let target = DownloadTarget {
                    board: Arc::clone(board),
                    index,
                };
                let fetched = fetcher
                    .fetch_input(
                        inputs[index].locked.clone(),
                        git_ssh_command.clone(),
                        target,
                    )
                    .await;
                let state = if fetched.is_ok() {
                    InputFetchState::Done
                } else {
                    InputFetchState::Failed
                };
                board.set_state(index, state);
                done.push((index, fetched));
            }
            done
        }
    };
    let fetches = futures::future::join_all(by_domain.into_values().map(fetch_domain));

    let snapshot = || {
        (!missing.is_empty()).then(|| EvalProgress::Fetching {
            inputs: board.snapshot(),
        })
    };
    let mut reporter = ChangeReporter::default();
    let fetched = tokio::select! {
        biased;
        () = abort.aborted() => anyhow::bail!("job aborted during flake input fetch"),
        fetched = fetches => fetched,
        never = reporter.run(sink, snapshot) => match never {},
    };
    reporter.flush(sink, &snapshot).await;

    let mut all: Vec<String> = paths.into_iter().filter(|p| !missing.contains(p)).collect();
    let mut warnings = Vec::new();
    for (index, result) in fetched.into_iter().flatten() {
        match result {
            Ok(path) => all.push(path),
            Err(e) => warnings.push(format!(
                "skipping flake input '{}': {}",
                inputs[index].name,
                format!("{e:#}").trim()
            )),
        }
    }
    all.sort();
    all.dedup();
    warnings.sort();
    Ok(FetchedInputs {
        paths: all,
        warnings,
        progress: snapshot(),
    })
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

    fn no_abort() -> AbortSignal {
        AbortSignal::never()
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
            &FakeFetcher::new(""),
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
            &FakeFetcher::new(""),
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
            &FakeFetcher::new(""),
            "ssh",
            no_abort(),
        )
        .await
        .unwrap();

        assert!(outcome.source_path.starts_with("/nix/store/"));
        assert!(outcome.source_path.ends_with("-source"));
        assert_eq!(outcome.input_paths, vec![outcome.source_path.clone()]);
        assert!(store.has_path(&outcome.source_path).await.unwrap());
        assert!(matches!(reporter.events()[0], ReportedEvent::Fetching));
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

    use crate::worker_pool::{DownloadTarget, InputFetcher};
    use gradient_util::sync::Mutex;
    use gradient_wire::types::{EvalProgress, InputFetchState};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn lock_with(nodes: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "root": "root", "version": 7, "nodes": nodes })
    }

    fn nar_hash(fill: char) -> String {
        format!("sha256-{}A=", fill.to_string().repeat(42))
    }

    fn github(repo: &str, hash: &str) -> serde_json::Value {
        serde_json::json!({ "locked": { "type": "github", "owner": "o", "repo": repo, "rev": "r", "narHash": hash } })
    }

    #[test]
    fn locked_inputs_skip_overridden_subtrees_and_name_by_root_input() {
        let lock = lock_with(serde_json::json!({
            "root": { "inputs": { "nixpkgs": "nixpkgs_2", "utils": "flake-utils", "home": "home" } },
            "nixpkgs_2": github("nixpkgs", &nar_hash('A')),
            "flake-utils": { "inputs": { "systems": "systems" },
                "locked": { "type": "tarball", "url": "https://codeload.github.com/x.tar.gz", "narHash": nar_hash('B') } },
            "systems": github("systems", &nar_hash('C')),
            "home": { "inputs": { "lib": "lib" }, "locked": { "type": "gitlab", "owner": "o", "repo": "h", "rev": "r",
                "narHash": nar_hash('D') } },
            "lib": github("lib", &nar_hash('E')),
            "stale": github("stale", &nar_hash('F')),
        }));
        let overridden = ["home".to_owned()].into_iter().collect();
        let inputs = locked_inputs(&lock, &overridden);
        let names: Vec<_> = inputs
            .iter()
            .map(|i| (i.name.as_str(), i.domain.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("nixpkgs", "github.com"),
                ("systems", "github.com"),
                ("utils", "github.com")
            ]
        );
        assert!(
            inputs
                .iter()
                .all(|i| i.store_path.starts_with("/nix/store/")
                    && i.store_path.ends_with("-source"))
        );
    }

    #[test]
    fn a_subdirectory_input_is_fetched_without_its_dir() {
        let lock = lock_with(serde_json::json!({
            "root": { "inputs": { "nixpkgs-lib": "nixpkgs-lib" } },
            "nixpkgs-lib": { "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs",
                "rev": "r", "dir": "lib", "narHash": nar_hash('A') } },
        }));
        let inputs = locked_inputs(&lock, &HashSet::new());
        let locked: serde_json::Value = serde_json::from_str(&inputs[0].locked).unwrap();
        assert_eq!(
            locked,
            serde_json::json!({ "type": "github", "owner": "NixOS", "repo": "nixpkgs",
                "rev": "r", "narHash": nar_hash('A') })
        );
    }

    #[test]
    fn domains_reduce_to_the_second_level() {
        let d = |v: serde_json::Value| input_domain(&v);
        assert_eq!(d(serde_json::json!({"type":"github"})), "github.com");
        assert_eq!(
            d(serde_json::json!({"type":"github","host":"git.corp.example.org"})),
            "example.org"
        );
        assert_eq!(d(serde_json::json!({"type":"sourcehut"})), "sr.ht");
        assert_eq!(
            d(serde_json::json!({"type":"git","url":"ssh://git@gitlab.com/a/b"})),
            "gitlab.com"
        );
        assert_eq!(
            d(serde_json::json!({"type":"tarball","url":"https://releases.nixos.org/x.tar.xz"})),
            "nixos.org"
        );
        assert_eq!(d(serde_json::json!({"type":"path","path":"/x"})), "");
    }

    struct FakeFetcher {
        in_flight: Mutex<HashMap<String, usize>>,
        overlapped: AtomicBool,
        fail: &'static str,
    }

    impl FakeFetcher {
        fn new(fail: &'static str) -> Self {
            Self {
                in_flight: Mutex::default(),
                overlapped: AtomicBool::default(),
                fail,
            }
        }
    }

    #[async_trait::async_trait]
    impl InputFetcher for FakeFetcher {
        async fn fetch_input(
            &self,
            locked: String,
            _: Option<String>,
            target: DownloadTarget,
        ) -> anyhow::Result<String> {
            let v: serde_json::Value = serde_json::from_str(&locked)?;
            let domain = input_domain(&v);
            {
                let mut in_flight = self.in_flight.lock();
                let n = in_flight.entry(domain.clone()).or_default();
                *n += 1;
                assert_eq!(*n, 1, "two downloads in flight for {domain}");
                if in_flight.values().filter(|n| **n > 0).count() > 1 {
                    self.overlapped.store(true, Ordering::SeqCst);
                }
            }
            target.board.transfer(target.index, 1, 10, 0);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            *self.in_flight.lock().get_mut(&domain).unwrap() -= 1;
            if v["repo"] == self.fail {
                anyhow::bail!("404");
            }
            Ok(format!("/nix/store/{}-source", v["repo"].as_str().unwrap()))
        }
    }

    fn input(name: &str, domain: &str, repo: &str) -> LockedInput {
        LockedInput {
            name: name.into(),
            domain: domain.into(),
            store_path: format!("/nix/store/{repo}-source"),
            locked: serde_json::json!({ "type": "github", "host": domain, "repo": repo })
                .to_string(),
        }
    }

    #[tokio::test]
    async fn domains_overlap_while_each_domain_stays_serial_and_a_failure_is_a_warning() {
        let fetcher = FakeFetcher::new("bad");
        let reporter = RecordingJobReporter::new();
        let store = FakeWorkerStore::default();
        let inputs = vec![
            input("a", "github.com", "a"),
            input("b", "github.com", "bad"),
            input("c", "gitlab.com", "c"),
        ];
        let FetchedInputs {
            paths, warnings, ..
        } = fetch_inputs(
            inputs,
            &store,
            &fetcher,
            &*reporter.eval_progress_sink(),
            None,
            &mut no_abort(),
        )
        .await
        .unwrap();
        assert!(fetcher.overlapped.load(Ordering::SeqCst));
        assert_eq!(paths.len(), 2);
        assert_eq!(warnings, vec!["skipping flake input 'b': 404".to_owned()]);
        let Some(ReportedEvent::EvalProgress(EvalProgress::Fetching { inputs })) =
            reporter.events().last().cloned()
        else {
            panic!("no final fetch snapshot");
        };
        let states: Vec<_> = inputs
            .iter()
            .map(|i| (i.name.as_str(), i.state, i.downloaded_bytes))
            .collect();
        assert_eq!(
            states,
            vec![
                ("a", InputFetchState::Done, 10),
                ("b", InputFetchState::Failed, 10),
                ("c", InputFetchState::Done, 10),
            ]
        );
    }

    #[tokio::test]
    async fn inputs_sharing_a_source_return_it_once() {
        let fetcher = FakeFetcher::new("");
        let reporter = RecordingJobReporter::new();
        let FetchedInputs { paths, .. } = fetch_inputs(
            vec![
                input("systems", "github.com", "systems"),
                input("systems_2", "github.com", "systems"),
            ],
            &FakeWorkerStore::default(),
            &fetcher,
            &*reporter.eval_progress_sink(),
            None,
            &mut no_abort(),
        )
        .await
        .unwrap();
        assert_eq!(paths, vec!["/nix/store/systems-source".to_owned()]);
    }

    #[tokio::test]
    async fn present_inputs_send_no_progress() {
        let fetcher = FakeFetcher::new("");
        let reporter = RecordingJobReporter::new();
        let store = FakeWorkerStore::new().with_present_path("/nix/store/a-source");
        let FetchedInputs { paths, .. } = fetch_inputs(
            vec![input("a", "github.com", "a")],
            &store,
            &fetcher,
            &*reporter.eval_progress_sink(),
            None,
            &mut no_abort(),
        )
        .await
        .unwrap();
        assert_eq!(paths, vec!["/nix/store/a-source".to_owned()]);
        assert!(reporter.events().is_empty());
    }

    #[tokio::test]
    async fn a_present_input_is_a_done_row_next_to_a_missing_one() {
        let fetcher = FakeFetcher::new("");
        let reporter = RecordingJobReporter::new();
        let store = FakeWorkerStore::new().with_present_path("/nix/store/a-source");
        let fetched = fetch_inputs(
            vec![input("a", "github.com", "a"), input("b", "github.com", "b")],
            &store,
            &fetcher,
            &*reporter.eval_progress_sink(),
            None,
            &mut no_abort(),
        )
        .await
        .unwrap();
        let Some(EvalProgress::Fetching { inputs }) = fetched.progress else {
            panic!("no final fetch snapshot");
        };
        let rows: Vec<_> = inputs
            .iter()
            .map(|i| (i.name.as_str(), i.state, i.downloaded_bytes))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("a", InputFetchState::Done, 0),
                ("b", InputFetchState::Done, 10),
            ]
        );
        let Some(ReportedEvent::EvalProgress(sent)) = reporter.events().last().cloned() else {
            panic!("the final snapshot was not sent");
        };
        assert_eq!(sent, EvalProgress::Fetching { inputs });
    }

    #[tokio::test]
    async fn no_rows_sends_no_progress() {
        let fetcher = FakeFetcher::new("");
        let reporter = RecordingJobReporter::new();
        let FetchedInputs { paths, .. } = fetch_inputs(
            vec![],
            &FakeWorkerStore::default(),
            &fetcher,
            &*reporter.eval_progress_sink(),
            None,
            &mut no_abort(),
        )
        .await
        .unwrap();
        assert!(paths.is_empty());
        assert!(reporter.events().is_empty());
    }

    #[tokio::test]
    async fn an_abort_stops_the_fetch() {
        let fetcher = FakeFetcher::new("");
        let reporter = RecordingJobReporter::new();
        let (tx, mut rx) = AbortSignal::channel();
        tx.send(true).unwrap();
        let err = fetch_inputs(
            vec![input("a", "github.com", "a")],
            &FakeWorkerStore::default(),
            &fetcher,
            &*reporter.eval_progress_sink(),
            None,
            &mut rx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("aborted"), "{err}");
    }
}
