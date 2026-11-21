/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gradient_sources::{GitOperationCredentials, SshGitPrivateKeyKnownHostsAndObservations};
use gradient_types::RepositoryUrl;
use gradient_types::fetch_source_allowlist_and_request_validation::{
    AllowedFetchRequest, ValidatedFlakeLock,
    check_flake_input_override_reference_is_remote_flake_or_single_store_path,
};
use gradient_util::sync::Mutex;
use gradient_wire::messages::{FlakeJob, FlakeSource};
use gradient_wire::traits::{EvalProgressSink, InputProgressSink, JobReporter, WorkerStore};
use gradient_wire::types::{EvalProgress, InputFetch, InputFetchState};
use tracing::{debug, info, warn};

use super::AbortSignal;
use super::capped_regular_file_below_trusted_root_reader::{
    SizeCappedNoFollowReadError, open_regular_file_below_root_and_read_at_most_max_bytes,
};
use super::dependencies::InputSource;
use super::progress_report::ChangeReporter;
use crate::error::WorkerError;
use crate::proto::credentials::JobCredentials;

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

#[derive(Debug)]
enum ValidatedFlakeSource<'a> {
    Repository { url: RepositoryUrl, commit: &'a str },
    Cached { store_path: &'a str },
}

impl<'a> ValidatedFlakeSource<'a> {
    fn of(source: &'a FlakeSource) -> Result<Self> {
        Ok(match source {
            FlakeSource::Repository { url, commit } => Self::Repository {
                url: url
                    .parse()
                    .with_context(|| format!("job repository `{url}`"))?,
                commit,
            },
            FlakeSource::Cached { store_path, .. } => Self::Cached { store_path },
        })
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn fetch_repository(
    job: &FlakeJob,
    updater: &mut dyn JobReporter,
    credentials: &JobCredentials,
    store: &dyn WorkerStore,
    abort: AbortSignal,
) -> Result<FetchOutcome> {
    let source = ValidatedFlakeSource::of(&job.source)?;
    fetch_flake_inputs_from_validated_job_source(job, source, updater, credentials, store, abort)
        .await
}

async fn fetch_flake_inputs_from_validated_job_source(
    job: &FlakeJob,
    source: ValidatedFlakeSource<'_>,
    updater: &mut dyn JobReporter,
    credentials: &JobCredentials,
    store: &dyn WorkerStore,
    abort: AbortSignal,
) -> Result<FetchOutcome> {
    let git_credentials = credentials.git_credentials_for_clone_and_fetch();
    let fetched = clone_job_repository_and_update_inputs_with_git_credentials(
        job,
        source,
        updater,
        &git_credentials,
        store,
        abort,
    )
    .await;
    if let Some(ssh) = &git_credentials.ssh_git_private_key_and_known_hosts {
        send_ssh_host_key_observations_or_log_warning(updater, ssh).await;
    }
    fetched
}

pub(crate) async fn send_ssh_host_key_observations_or_log_warning(
    updater: &mut dyn JobReporter,
    ssh: &SshGitPrivateKeyKnownHostsAndObservations,
) {
    if let Err(error) = updater
        .report_git_ssh_host_key_observations(ssh.observations())
        .await
    {
        warn!(%error, "The job doesn't send host key observations.");
    }
}

async fn clone_job_repository_and_update_inputs_with_git_credentials(
    job: &FlakeJob,
    source: ValidatedFlakeSource<'_>,
    updater: &mut dyn JobReporter,
    git_credentials: &GitOperationCredentials,
    store: &dyn WorkerStore,
    mut abort: AbortSignal,
) -> Result<FetchOutcome> {
    if abort.is_aborted() {
        anyhow::bail!("job aborted");
    }

    updater.report_fetching().await?;

    let (source_path, flake_root, lock, _checkout) = match source {
        ValidatedFlakeSource::Repository { url, commit } => {
            debug!(
                url = url.git_connection_url_for_log_without_username_password_and_query(),
                %commit,
                has_ssh_key = git_credentials.ssh_git_private_key_and_known_hosts.is_some(),
                "fetching repository"
            );
            let commit = commit.to_owned();

            let git_credentials_for_clone = git_credentials.clone();
            let commit_for_clone = commit.clone();
            let clone_task = tokio::task::spawn_blocking(move || {
                gradient_sources::fetch_commit_into_new_bare_temporary_repository(
                    &url,
                    &commit_for_clone,
                    &git_credentials_for_clone,
                )
            });

            let fetched = tokio::select! {
                biased;
                () = abort.aborted() => {
                    anyhow::bail!("job aborted during git clone");
                }
                result = clone_task => {
                    result.context("fetch task panicked")??
                }
            };
            let repository_path = fetched.repository().path().to_string_lossy().into_owned();
            let flake_root = flake_root(Path::new(&commit), job.flake_dir.as_deref())?;
            let (lock_commit, flake_dir) = (commit.clone(), job.flake_dir.clone());
            let (fetched, lock_bytes) = tokio::task::spawn_blocking(move || {
                let lock_bytes = super::source::read_regular_flake_lock_blob_from_commit_tree(
                    &fetched,
                    &lock_commit,
                    flake_dir.as_deref(),
                    MAX_FLAKE_LOCK_FILE_SIZE_BYTES,
                );
                (fetched, lock_bytes)
            })
            .await
            .context("`flake.lock` read task panicked")?;
            let lock_bytes = lock_bytes?;
            let mut lock = parse_and_validate_optional_flake_lock(lock_bytes, job)?;

            if let Some(spec) = &job.input_update
                && let Some(updated) =
                    run_input_update(spec, lock.as_ref(), git_credentials, updater, &mut abort)
                        .await?
            {
                lock = parse_and_validate_optional_flake_lock(Some(updated), job)?;
            }

            let source_path = super::source::add_git_tree(store, &repository_path, &commit).await?;
            (source_path, flake_root, lock, Some(fetched))
        }
        ValidatedFlakeSource::Cached { store_path } => {
            debug!(%store_path, "using the cached build source");
            let flake_root = flake_root(Path::new(store_path), job.flake_dir.as_deref())?;
            let lock_bytes = read_optional_flake_lock_file_from_local_store_path(
                store_path,
                job.flake_dir.as_deref(),
            )
            .await?;
            let lock = parse_and_validate_optional_flake_lock(lock_bytes, job)?;
            (store_path.to_owned(), flake_root, lock, None)
        }
    };

    let overrides_in: Vec<OverrideInput> = job.input_overrides.iter().map(Into::into).collect();
    let (applied_overrides, warnings) = match (&lock, overrides_in.is_empty()) {
        (_, true) => (Vec::new(), Vec::new()),
        (Some(lock), false) => {
            let declared = declared_inputs_from_lock(lock.parsed_json_value())?;
            resolve_overrides(&overrides_in, &declared, lock.parsed_json_value())?
        }
        (None, false) => anyhow::bail!(
            "input overrides need a flake.lock in {}",
            flake_root.display()
        ),
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
    let inputs = match &lock {
        Some(lock) => locked_inputs(lock, &overridden)?,
        None => Vec::new(),
    };
    let sink = updater.eval_progress_sink();
    let fetched = {
        let source = InputSource::new(&*updater);
        fetch_inputs(inputs, store, &source, &*sink, &mut abort).await?
    };
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
    store_path: String,
    request: String,
}

pub(super) fn flake_root(base: &Path, flake_dir: Option<&str>) -> Result<PathBuf> {
    let Some(dir) = flake_dir else {
        return Ok(base.to_path_buf());
    };
    gradient_types::check_flake_dir(dir)?;
    Ok(base.join(dir))
}

fn parse_and_validate_optional_flake_lock(
    bytes: Option<Vec<u8>>,
    job: &FlakeJob,
) -> Result<Option<ValidatedFlakeLock>> {
    Ok(bytes
        .map(|bytes| ValidatedFlakeLock::parse(bytes, job.flake_dir.as_deref()))
        .transpose()?)
}

const MAX_FLAKE_LOCK_FILE_SIZE_BYTES: u64 = 16 * 1024 * 1024;

pub(super) async fn read_optional_flake_lock_file_from_local_store_path(
    store_path: &str,
    flake_dir: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    let relative = Path::new(flake_dir.unwrap_or("")).join("flake.lock");
    match open_regular_file_below_root_and_read_at_most_max_bytes(
        Path::new(store_path),
        &relative,
        MAX_FLAKE_LOCK_FILE_SIZE_BYTES,
    )
    .await
    {
        Ok(bytes) => Ok(Some(bytes)),
        Err(SizeCappedNoFollowReadError::FileOrAncestorDirectoryNotFound { .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn locked_inputs(
    validated: &ValidatedFlakeLock,
    overridden: &HashSet<String>,
) -> Result<Vec<LockedInput>> {
    let lock = validated.parsed_json_value();
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
        let Some(attributes) = validated.validated_fetch_tree_input_by_node_key(key) else {
            continue;
        };
        let Some(store_path) = node["locked"]["narHash"]
            .as_str()
            .and_then(source_store_path)
        else {
            continue;
        };
        let request =
            AllowedFetchRequest::build_accepted_fetch_tree_request_from_validated_attributes_and_expected_store_path(
                attributes.clone(),
                Some(store_path.clone()),
            )?;
        inputs.push(LockedInput {
            name: root_names.get(key).copied().unwrap_or(key).to_owned(),
            request: request.get_canonical_fetch_request_json(),
            store_path,
        });
    }
    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(inputs)
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

#[derive(Debug)]
struct InputRow {
    name: String,
    state: InputFetchState,
    downloaded_bytes: u64,
    expected_bytes: Option<u64>,
}

#[derive(Debug)]
struct InputBoard {
    rows: Mutex<Vec<InputRow>>,
}

impl InputBoard {
    fn new(rows: Vec<(String, InputFetchState)>) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(Self::rows(rows)),
        })
    }

    fn rows(rows: impl IntoIterator<Item = (String, InputFetchState)>) -> Vec<InputRow> {
        rows.into_iter()
            .map(|(name, state)| InputRow {
                name,
                state,
                downloaded_bytes: 0,
                expected_bytes: None,
            })
            .collect()
    }

    fn set_state(&self, index: usize, state: InputFetchState) {
        self.rows.lock()[index].state = state;
    }

    fn transfer(&self, index: usize, bytes_done: u64, bytes_total: Option<u64>) {
        let row = &mut self.rows.lock()[index];
        row.downloaded_bytes = bytes_done;
        row.expected_bytes = bytes_total;
    }

    fn snapshot(&self) -> Vec<InputFetch> {
        self.rows
            .lock()
            .iter()
            .map(|row| InputFetch {
                name: row.name.clone(),
                state: row.state,
                downloaded_bytes: row.downloaded_bytes,
                expected_bytes: row.expected_bytes.unwrap_or_default(),
            })
            .collect()
    }
}

struct BoardRow {
    board: Arc<InputBoard>,
    index: usize,
}

impl InputProgressSink for BoardRow {
    fn transfer(&self, bytes_done: u64, bytes_total: Option<u64>) {
        self.board.transfer(self.index, bytes_done, bytes_total);
    }
}

struct ResolveBoard(Arc<InputBoard>);

impl ResolveBoard {
    fn progress(&self) -> Option<EvalProgress> {
        let inputs = self.0.snapshot();
        (!inputs.is_empty()).then_some(EvalProgress::Fetching { inputs })
    }
}

impl gradient_sources::flake_lock::ResolveProgress for ResolveBoard {
    fn queued(&self, inputs: &[String]) {
        let queued = inputs
            .iter()
            .map(|name| (name.clone(), InputFetchState::Queued));
        *self.0.rows.lock() = InputBoard::rows(queued);
    }

    fn started(&self, index: usize) -> Arc<dyn InputProgressSink> {
        self.0.set_state(index, InputFetchState::Fetching);
        Arc::new(BoardRow {
            board: Arc::clone(&self.0),
            index,
        })
    }

    fn finished(&self, index: usize, resolved: bool) {
        let state = if resolved {
            InputFetchState::Done
        } else {
            InputFetchState::Failed
        };
        self.0.set_state(index, state);
    }
}

#[tracing::instrument(level = "debug", skip_all, fields(inputs = inputs.len()))]
async fn fetch_inputs(
    inputs: Vec<LockedInput>,
    store: &dyn WorkerStore,
    source: &InputSource<'_>,
    sink: &dyn EvalProgressSink,
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

    let fetch_one = |index: usize| {
        let (board, inputs) = (&board, &inputs);
        async move {
            board.set_state(index, InputFetchState::Fetching);
            let progress = Arc::new(BoardRow {
                board: Arc::clone(board),
                index,
            });
            let fetched = source.fetch(inputs[index].request.clone(), progress).await;
            let state = if fetched.is_ok() {
                InputFetchState::Done
            } else {
                InputFetchState::Failed
            };
            board.set_state(index, state);
            (index, fetched)
        }
    };
    let fetches = futures::future::join_all(
        (0..inputs.len())
            .filter(|&i| missing.contains(&inputs[i].store_path))
            .map(fetch_one),
    );

    let snapshot = || {
        (!missing.is_empty()).then(|| EvalProgress::Fetching {
            inputs: board.snapshot(),
        })
    };
    let mut reporter = ChangeReporter::default();
    let fetched = tokio::select! {
        biased;
        () = abort.aborted() => anyhow::bail!(WorkerError::FlakeInputFetchAborted),
        fetched = fetches => fetched,
        never = reporter.run(sink, snapshot) => match never {},
    };
    reporter.flush(sink, &snapshot).await;

    let mut all: Vec<String> = paths.into_iter().filter(|p| !missing.contains(p)).collect();
    let mut warnings = Vec::new();
    for (index, result) in fetched {
        match result {
            Ok(_) => all.push(inputs[index].store_path.clone()),
            Err(message) => warnings.push(format!(
                "skipping flake input '{}': {}",
                inputs[index].name,
                message.trim()
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
    lock: Option<&ValidatedFlakeLock>,
    git_credentials: &GitOperationCredentials,
    updater: &mut dyn JobReporter,
    abort: &mut AbortSignal,
) -> Result<Option<Vec<u8>>> {
    let Some(lock) = lock else {
        anyhow::bail!("input update: flake.lock missing from commit");
    };
    let lock_bytes = lock.raw_lock_file_bytes();
    if spec.discover_only {
        let declared = declared_inputs_from_lock(lock.parsed_json_value())?;
        let mut matched: Vec<String> = spec
            .inputs
            .iter()
            .filter(|p| gradient_util::glob::is_pattern(p))
            .flat_map(|p| {
                declared
                    .iter()
                    .filter(move |d| gradient_wildcard::candidate_matches_wildcard_pattern(p, d))
                    .cloned()
            })
            .collect();
        matched.sort();
        matched.dedup();
        return updater.report_input_expansion(matched).await.map(|()| None);
    }

    let tracked: Vec<gradient_sources::flake_lock::InputName> =
        spec.inputs.iter().cloned().map(Into::into).collect();
    gradient_sources::flake_lock::check_each_tracked_input_is_a_remote_direct_input(
        lock_bytes, &tracked,
    )?;
    report_input_sources(&spec.inputs, lock_bytes, updater).await;
    let sink = updater.eval_progress_sink();
    let Some(patch) =
        resolve_newest_revisions(spec, lock_bytes, git_credentials, &*sink, abort).await?
    else {
        return Ok(None);
    };

    let updated_lock = patch
        .edits
        .into_iter()
        .find(|e| e.path.as_os_str() == "flake.lock")
        .map(|e| e.contents);
    let candidate = updated_lock
        .as_deref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
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

    updater.report_input_update(candidate, bumped).await?;
    Ok(updated_lock)
}

async fn report_input_sources(tracked: &[String], lock: &[u8], updater: &mut dyn JobReporter) {
    let sent = match read_input_sources(tracked, lock) {
        Ok(sources) => updater.report_input_sources(sources).await,
        Err(e) => Err(e),
    };
    if let Err(e) = sent {
        tracing::warn!(error = %e, "Skip reports of repositories for tracked flake inputs");
    }
}

fn read_input_sources(
    tracked: &[String],
    lock: &[u8],
) -> Result<Vec<gradient_wire::messages::FlakeInputSourceWire>> {
    use gradient_sources::flake_lock::{FlakeLock, input_source::tracked_input_sources};

    let lock = FlakeLock::parse(lock)?;
    Ok(tracked_input_sources(&lock, tracked)
        .into_iter()
        .map(|s| gradient_wire::messages::FlakeInputSourceWire {
            name: s.name,
            repository_url: s.repository_url,
            branch: s.branch,
        })
        .collect())
}

async fn resolve_newest_revisions(
    spec: &gradient_wire::messages::InputUpdateSpec,
    lock: &[u8],
    git_credentials: &GitOperationCredentials,
    sink: &dyn EvalProgressSink,
    abort: &mut AbortSignal,
) -> Result<Option<gradient_sources::flake_lock::Patch>> {
    use gradient_sources::flake_lock::PatchGenerator as _;

    let resolver = gradient_sources::flake_lock::HttpRevisionResolver::new(
        gradient_worker_client::http::download_client().clone(),
    )
    .with_git_credentials(git_credentials.clone());
    let generator = gradient_sources::flake_lock::FlakeLockGenerator::new(resolver);
    let tracked: Vec<gradient_sources::flake_lock::InputName> =
        spec.inputs.iter().cloned().map(Into::into).collect();

    let board = ResolveBoard(InputBoard::new(Vec::new()));
    let snapshot = || board.progress();
    let mut reporter = ChangeReporter::default();
    let patch = tokio::select! {
        biased;
        () = abort.aborted() => anyhow::bail!(WorkerError::FlakeInputFetchAborted),
        patch = generator.produce(lock, &tracked, &board) => patch,
        never = reporter.run(sink, snapshot) => match never {},
    };
    reporter.flush(sink, &snapshot).await;
    patch.context("run the flake.lock update generator")
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
        check_flake_input_override_reference_is_remote_flake_or_single_store_path(
            &input_name,
            &ref_str,
        )?;
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
            flake_dir: None,
            run_overrides: None,
            nix_evaluation_fetch_url_access: Default::default(),
        }
    }

    fn no_abort() -> AbortSignal {
        AbortSignal::never()
    }

    #[tokio::test]
    async fn fetch_reports_fetching_and_succeeds() {
        let job = make_flake_job();
        let credentials = crate::proto::credentials::JobCredentials::default();
        let mut reporter = RecordingJobReporter::new();

        let result = fetch_repository(
            &job,
            &mut reporter,
            &credentials,
            &FakeWorkerStore::new(),
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
                commit: None,
            },
            wildcards: vec![],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
            flake_dir: None,
            run_overrides: None,
            nix_evaluation_fetch_url_access: Default::default(),
        };
        let credentials = crate::proto::credentials::JobCredentials::default();
        let mut reporter = RecordingJobReporter::new();
        let result = fetch_repository(
            &job,
            &mut reporter,
            &credentials,
            &FakeWorkerStore::new(),
            no_abort(),
        )
        .await;
        let msg = format!("{:?}", result.err());
        assert!(
            !msg.contains("requires FlakeSource::Repository"),
            "cached must be handled: {msg}"
        );
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn committed_repo(repo: &Path, files: &[(&str, &str)]) -> String {
        std::fs::create_dir_all(repo).unwrap();
        git(repo, &["init", "-b", "main"]);
        for (path, contents) in files {
            let file = repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
        }
        git(repo, &["add", "."]);
        git(
            repo,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "init",
            ],
        );
        git(repo, &["rev-parse", "HEAD"])
    }

    fn repository_job(repo: &Path, commit: String) -> FlakeJob {
        FlakeJob {
            source: FlakeSource::Repository {
                url: format!("file://{}", repo.display()),
                commit,
            },
            ..make_flake_job()
        }
    }

    async fn fetch_job_from_local_test_repository(
        job: &FlakeJob,
        reporter: &mut RecordingJobReporter,
        credentials: &JobCredentials,
        store: &FakeWorkerStore,
    ) -> Result<FetchOutcome> {
        let source = match &job.source {
            FlakeSource::Repository { url, commit } if url.starts_with("file://") => {
                ValidatedFlakeSource::Repository {
                    url: RepositoryUrl::local_test_repository(Path::new(&url["file://".len()..])),
                    commit,
                }
            }
            other => ValidatedFlakeSource::of(other)?,
        };
        fetch_flake_inputs_from_validated_job_source(
            job,
            source,
            reporter,
            credentials,
            store,
            no_abort(),
        )
        .await
    }

    async fn fetch(job: &FlakeJob, store: &FakeWorkerStore) -> Result<FetchOutcome> {
        let credentials = JobCredentials::default();
        let mut reporter = RecordingJobReporter::new();
        fetch_job_from_local_test_repository(job, &mut reporter, &credentials, store).await
    }

    #[tokio::test]
    async fn fetch_fails_for_local_repository_url_before_git_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        let sha = committed_repo(&repo_dir, &[("flake.nix", "{}")]);
        for url in [
            format!("file://{}", repo_dir.display()),
            repo_dir.display().to_string(),
        ] {
            let job = FlakeJob {
                source: FlakeSource::Repository {
                    url: url.clone(),
                    commit: sha.clone(),
                },
                ..make_flake_job()
            };
            let mut reporter = RecordingJobReporter::new();
            let store = FakeWorkerStore::new();
            let result = fetch_repository(
                &job,
                &mut reporter,
                &JobCredentials::default(),
                &store,
                no_abort(),
            )
            .await;
            assert!(result.is_err(), "{url}");
            assert_eq!(reporter.len(), 0, "{url}");
        }
    }

    #[tokio::test]
    async fn fetch_repository_adds_the_clone_to_the_store_without_nix() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        let sha = committed_repo(&repo_dir, &[("flake.nix", "{}")]);
        assert!(sha.len() == 40, "expected 40-char SHA, got: {sha}");
        let job = repository_job(&repo_dir, sha);

        let credentials = crate::proto::credentials::JobCredentials::default();
        let mut reporter = RecordingJobReporter::new();
        let store = FakeWorkerStore::new();

        let outcome =
            fetch_job_from_local_test_repository(&job, &mut reporter, &credentials, &store)
                .await
                .unwrap();

        assert!(outcome.source_path.starts_with("/nix/store/"));
        assert!(outcome.source_path.ends_with("-source"));
        assert_eq!(outcome.input_paths, vec![outcome.source_path.clone()]);
        assert!(store.has_path(&outcome.source_path).await.unwrap());
        assert!(matches!(reporter.events()[0], ReportedEvent::Fetching));
    }

    #[tokio::test]
    async fn a_subdirectory_flake_reads_its_own_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        let sha = committed_repo(
            &repo_dir,
            &[
                ("flake.nix", "{}"),
                ("sub/flake.nix", "{}"),
                (
                    "sub/flake.lock",
                    r#"{"nodes":{"root":{}},"root":"root","version":7}"#,
                ),
            ],
        );
        let in_sub = FlakeJob {
            input_overrides: vec![gradient_wire::types::FlakeInputOverride {
                input_name: "missing".into(),
                url: None,
            }],
            flake_dir: Some("sub".into()),
            ..repository_job(&repo_dir, sha)
        };
        let at_root = FlakeJob {
            flake_dir: None,
            ..in_sub.clone()
        };

        fetch(&in_sub, &FakeWorkerStore::new()).await.unwrap();
        let Err(err) = fetch(&at_root, &FakeWorkerStore::new()).await else {
            panic!("a root flake without a lock takes no overrides")
        };
        assert!(
            err.to_string()
                .contains("input overrides need a flake.lock"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn an_escaping_flake_directory_fails_the_fetch() {
        let job = FlakeJob {
            source: FlakeSource::Cached {
                store_path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source".into(),
                commit: None,
            },
            flake_dir: Some("../x".into()),
            ..make_flake_job()
        };
        let Err(err) = fetch(&job, &FakeWorkerStore::new()).await else {
            panic!("an escaping flake directory is refused")
        };
        assert_eq!(
            err.to_string(),
            gradient_types::InputError::InvalidFlakeDir("../x".into()).to_string()
        );
    }

    #[tokio::test]
    async fn fetch_fails_quickly_when_flake_lock_is_symlink_to_dev_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::os::unix::fs::symlink("/dev/zero", repo_dir.join("flake.lock")).unwrap();
        let sha = committed_repo(&repo_dir, &[("flake.nix", "{}")]);
        let job = repository_job(&repo_dir, sha);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fetch(&job, &FakeWorkerStore::new()),
        )
        .await
        .expect("the fetch must not read from /dev/zero");

        let Err(err) = outcome else {
            panic!("a symlinked flake.lock is refused")
        };
        assert!(err.to_string().contains("is not a regular file"), "{err:#}");
    }

    fn cached_job(store_path: &Path) -> FlakeJob {
        FlakeJob {
            source: FlakeSource::Cached {
                store_path: store_path.display().to_string(),
                commit: None,
            },
            ..make_flake_job()
        }
    }

    #[tokio::test]
    async fn fetch_fails_quickly_when_cached_build_source_store_path_flake_lock_is_symlink_to_dev_zero()
     {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("flake.nix"), "{}").unwrap();
        std::os::unix::fs::symlink("/dev/zero", source.path().join("flake.lock")).unwrap();

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fetch(&cached_job(source.path()), &FakeWorkerStore::new()),
        )
        .await
        .expect("the fetch must not read from /dev/zero");

        let Err(err) = outcome else {
            panic!("a symlinked flake.lock is refused")
        };
        assert!(
            matches!(
                err.downcast_ref::<SizeCappedNoFollowReadError>(),
                Some(SizeCappedNoFollowReadError::PathComponentIsSymlinkOrIntermediateComponentIsNotDirectory { .. })
            ),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn cached_build_source_store_path_with_flake_lock_over_byte_cap_fails_fetch() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("flake.nix"), "{}").unwrap();
        let oversized = vec![b' '; usize::try_from(MAX_FLAKE_LOCK_FILE_SIZE_BYTES).unwrap() + 1];
        std::fs::write(source.path().join("flake.lock"), oversized).unwrap();

        let Err(err) = fetch(&cached_job(source.path()), &FakeWorkerStore::new()).await else {
            panic!("an oversized flake.lock is refused")
        };
        assert!(
            matches!(
                err.downcast_ref::<SizeCappedNoFollowReadError>(),
                Some(SizeCappedNoFollowReadError::FileIsLargerThanByteCap { .. })
            ),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn committed_flake_lock_over_byte_cap_fails_fetch() {
        let oversized = " ".repeat(usize::try_from(MAX_FLAKE_LOCK_FILE_SIZE_BYTES).unwrap() + 1);

        let (outcome, reporter) = commit_flake_lock_and_fetch_repository(&oversized).await;

        let Err(err) = outcome else {
            panic!("an oversized flake.lock is refused")
        };
        assert!(err.to_string().contains("is larger than"), "{err:#}");
        assert!(reporter.input_requests().is_empty(), "no InputRequest");
    }

    #[tokio::test]
    async fn fetch_fails_for_committed_flake_lock_with_local_git_repository_or_file_tarball_inputs_and_sends_no_input_requests()
     {
        for (locked, original) in [
            (
                serde_json::json!({
                    "type": "git", "url": "file:///srv/repo",
                    "rev": "1111111111111111111111111111111111111111", "narHash": nar_hash('A'),
                }),
                serde_json::json!({ "type": "git", "url": "file:///srv/repo" }),
            ),
            (
                serde_json::json!({ "type": "tarball", "url": "file:///etc/passwd", "narHash": nar_hash('A') }),
                serde_json::json!({ "type": "tarball", "url": "file:///etc/passwd" }),
            ),
        ] {
            let lock = serde_json::json!({
                "nodes": {
                    "root": { "inputs": { "dep": "dep" } },
                    "dep": { "locked": locked, "original": original },
                },
                "root": "root",
                "version": 7,
            });

            let (outcome, reporter) =
                commit_flake_lock_and_fetch_repository(&lock.to_string()).await;

            let Err(err) = outcome else {
                panic!("a local input is refused: {locked}")
            };
            assert!(
                err.downcast_ref::<gradient_types::InputError>().is_some(),
                "{err:#}"
            );
            assert!(reporter.input_requests().is_empty(), "no InputRequest");
        }
    }

    async fn commit_flake_lock_and_fetch_repository(
        lock: &str,
    ) -> (Result<FetchOutcome>, RecordingJobReporter) {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        let sha = committed_repo(
            &repo_dir,
            &[
                ("flake.nix", "{}"),
                ("sub/flake.nix", "{}"),
                ("flake.lock", lock),
            ],
        );
        let credentials = crate::proto::credentials::JobCredentials::default();
        let mut reporter = RecordingJobReporter::new();
        let outcome = fetch_job_from_local_test_repository(
            &repository_job(&repo_dir, sha),
            &mut reporter,
            &credentials,
            &FakeWorkerStore::new(),
        )
        .await;
        (outcome, reporter)
    }

    #[tokio::test]
    async fn fetch_fails_for_flake_lock_path_node_with_absolute_host_path_and_sends_no_input_requests()
     {
        let lock = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "secret": "secret" } },
                "secret": {
                    "locked": { "type": "path", "path": "/run/credentials/x", "narHash": nar_hash('A') },
                    "original": { "type": "path", "path": "/run/credentials/x" },
                },
            },
            "root": "root",
            "version": 7,
        });

        let (outcome, reporter) = commit_flake_lock_and_fetch_repository(&lock.to_string()).await;

        let Err(err) = outcome else {
            panic!("an absolute path node is refused")
        };
        assert!(
            err.downcast_ref::<gradient_types::InputError>().is_some(),
            "{err:#}"
        );
        assert!(reporter.input_requests().is_empty(), "no InputRequest");
    }

    #[tokio::test]
    async fn committed_flake_lock_relative_path_node_succeeds_without_input_request() {
        let lock = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "sub": "sub" } },
                "sub": {
                    "locked": { "type": "path", "path": "./sub" },
                    "original": { "type": "path", "path": "./sub" },
                    "parent": [],
                },
            },
            "root": "root",
            "version": 7,
        });

        let (outcome, reporter) = commit_flake_lock_and_fetch_repository(&lock.to_string()).await;

        outcome.unwrap();
        assert!(reporter.input_requests().is_empty(), "no InputRequest");
    }

    #[tokio::test]
    async fn file_url_tracked_input_update_fails_before_clone_and_reports_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let host_repo = tmp.path().join("host");
        committed_repo(&host_repo, &[("secret", "host content")]);
        let lock = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "dep": "dep" } },
                "dep": {
                    "locked": {
                        "type": "github", "owner": "o", "repo": "r",
                        "rev": "1111111111111111111111111111111111111111",
                        "narHash": nar_hash('A'),
                    },
                    "original": { "type": "git", "url": format!("file://{}", host_repo.display()) },
                },
            },
            "root": "root",
            "version": 7,
        });
        let repo_dir = tmp.path().join("repo");
        let sha = committed_repo(
            &repo_dir,
            &[("flake.nix", "{}"), ("flake.lock", &lock.to_string())],
        );
        let job = FlakeJob {
            input_update: Some(gradient_wire::messages::InputUpdateSpec {
                generator: "flake-lock".into(),
                inputs: vec!["dep".into()],
                discover_only: false,
            }),
            ..repository_job(&repo_dir, sha)
        };
        let credentials = crate::proto::credentials::JobCredentials::default();
        let mut reporter = RecordingJobReporter::new();

        let outcome = fetch_job_from_local_test_repository(
            &job,
            &mut reporter,
            &credentials,
            &FakeWorkerStore::new(),
        )
        .await;

        let Err(err) = outcome else {
            panic!("a local repository in `original` is refused")
        };
        assert!(
            format!("{err:#}").contains("doesn't point to a remote source"),
            "{err:#}"
        );
        let events = reporter.events();
        assert!(
            !events.iter().any(|event| matches!(
                event,
                ReportedEvent::InputUpdateReported { .. }
                    | ReportedEvent::InputSourcesReported { .. }
            )),
            "{events:?}"
        );
        assert!(
            !format!("{events:?}").contains(&*host_repo.to_string_lossy()),
            "{events:?}"
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
    fn rejects_job_input_override_that_uses_original_lock_reference_to_local_path() {
        let declared: std::collections::HashSet<String> = ["secret".to_owned()].into();
        let lock = serde_json::json!({
            "nodes": {
                "root": {"inputs": {"secret": "secret"}},
                "secret": {"original": {"type":"path","path":"/run/credentials/x"}},
            },
            "root": "root",
        });
        let overrides = [super::OverrideInput {
            input_name: "secret".into(),
            url: None,
        }];

        let Err(err) = super::resolve_overrides(&overrides, &declared, &lock) else {
            panic!("a local path override is refused")
        };

        assert!(
            err.downcast_ref::<gradient_types::InputError>().is_some(),
            "{err:#}"
        );
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

    use gradient_wire::types::InputOutcome;

    fn lock_with(nodes: serde_json::Value) -> ValidatedFlakeLock {
        let lock = serde_json::json!({ "root": "root", "version": 7, "nodes": nodes });
        ValidatedFlakeLock::parse(serde_json::to_vec(&lock).unwrap(), None).unwrap()
    }

    fn valid_nix_store_path_for_repository_name(repo: &str) -> String {
        format!("/nix/store/{}-{repo}-source", "0".repeat(32))
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
        let inputs = locked_inputs(&lock, &overridden).unwrap();
        let names: Vec<_> = inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["nixpkgs", "systems", "utils"]);
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
        let inputs = locked_inputs(&lock, &HashSet::new()).unwrap();
        let request: serde_json::Value = serde_json::from_str(&inputs[0].request).unwrap();
        assert_eq!(
            request,
            serde_json::json!({ "builtin": "fetchTree", "storePath": inputs[0].store_path,
                "args": { "type": "github", "owner": "NixOS", "repo": "nixpkgs",
                "rev": "r", "narHash": nar_hash('A') } })
        );
    }

    fn input(name: &str, repo: &str) -> LockedInput {
        let store_path = valid_nix_store_path_for_repository_name(repo);
        LockedInput {
            name: name.into(),
            request: serde_json::json!({ "builtin": "fetchTree",
                "args": { "type": "github", "repo": repo }, "storePath": store_path })
            .to_string(),
            store_path,
        }
    }

    fn fetched(repo: &str) -> InputOutcome {
        InputOutcome::Fetched {
            response:
                serde_json::json!({ "storePath": valid_nix_store_path_for_repository_name(repo) })
                    .to_string(),
        }
    }

    async fn fetch_with(
        reporter: &RecordingJobReporter,
        store: &FakeWorkerStore,
        inputs: Vec<LockedInput>,
        abort: &mut AbortSignal,
    ) -> Result<FetchedInputs> {
        let source = InputSource::new(reporter);
        fetch_inputs(
            inputs,
            store,
            &source,
            &*reporter.eval_progress_sink(),
            abort,
        )
        .await
    }

    fn rows(progress: Option<EvalProgress>) -> Vec<(String, InputFetchState, u64, u64)> {
        let Some(EvalProgress::Fetching { inputs }) = progress else {
            panic!("no fetch snapshot");
        };
        inputs
            .into_iter()
            .map(|i| (i.name, i.state, i.downloaded_bytes, i.expected_bytes))
            .collect()
    }

    #[tokio::test]
    async fn locked_inputs_are_requested_up_front_and_failures_become_warnings() {
        let reporter = RecordingJobReporter::new()
            .with_input_outcome(&valid_nix_store_path_for_repository_name("a"), fetched("a"))
            .with_input_outcome(
                &valid_nix_store_path_for_repository_name("bad"),
                InputOutcome::Failed {
                    message: "404 ".into(),
                },
            );
        let fetched = fetch_with(
            &reporter,
            &FakeWorkerStore::default(),
            vec![input("a", "a"), input("b", "bad")],
            &mut no_abort(),
        )
        .await
        .unwrap();

        assert_eq!(
            fetched.paths,
            vec![valid_nix_store_path_for_repository_name("a")]
        );
        assert_eq!(
            fetched.warnings,
            vec!["skipping flake input 'b': 404".to_owned()]
        );
        assert_eq!(reporter.input_requests().len(), 2);
        let states: Vec<_> = rows(fetched.progress)
            .into_iter()
            .map(|(name, state, ..)| (name, state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("a".to_owned(), InputFetchState::Done),
                ("b".to_owned(), InputFetchState::Failed)
            ]
        );
    }

    #[test]
    fn resolving_inputs_fill_the_input_board() {
        use gradient_sources::flake_lock::ResolveProgress as _;

        let board = ResolveBoard(InputBoard::new(Vec::new()));
        assert_eq!(board.progress(), None);

        board.queued(&["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        board.started(0).transfer(40, Some(90));
        board.finished(0, true);
        board.started(1).transfer(7, None);
        board.started(2);
        board.finished(2, false);

        assert_eq!(
            rows(board.progress()),
            vec![
                ("a".to_owned(), InputFetchState::Done, 40, 90),
                ("b".to_owned(), InputFetchState::Fetching, 7, 0),
                ("c".to_owned(), InputFetchState::Failed, 0, 0),
            ]
        );
    }

    #[tokio::test]
    async fn an_input_in_the_store_is_answered_without_the_server() {
        let reporter = RecordingJobReporter::new();
        let store =
            FakeWorkerStore::new().with_present_path(valid_nix_store_path_for_repository_name("a"));
        let fetched = fetch_with(&reporter, &store, vec![input("a", "a")], &mut no_abort())
            .await
            .unwrap();

        assert_eq!(
            fetched.paths,
            vec![valid_nix_store_path_for_repository_name("a")]
        );
        assert!(reporter.events().is_empty(), "{:?}", reporter.events());
    }

    #[tokio::test]
    async fn server_progress_moves_the_input_board() {
        let reporter = RecordingJobReporter::new()
            .with_input_outcome(&valid_nix_store_path_for_repository_name("b"), fetched("b"))
            .with_input_progress(40, Some(90));
        let store =
            FakeWorkerStore::new().with_present_path(valid_nix_store_path_for_repository_name("a"));
        let fetched = fetch_with(
            &reporter,
            &store,
            vec![input("a", "a"), input("b", "b")],
            &mut no_abort(),
        )
        .await
        .unwrap();

        assert_eq!(
            rows(fetched.progress),
            vec![
                ("a".to_owned(), InputFetchState::Done, 0, 0),
                ("b".to_owned(), InputFetchState::Done, 40, 90),
            ]
        );
        let Some(ReportedEvent::EvalProgress(sent)) = reporter.events().last().cloned() else {
            panic!("the final snapshot was not sent");
        };
        assert!(matches!(sent, EvalProgress::Fetching { .. }));
    }

    #[tokio::test]
    async fn inputs_sharing_a_source_are_requested_once() {
        let reporter = RecordingJobReporter::new().with_input_outcome(
            &valid_nix_store_path_for_repository_name("systems"),
            fetched("systems"),
        );
        let fetched = fetch_with(
            &reporter,
            &FakeWorkerStore::default(),
            vec![input("systems", "systems"), input("systems_2", "systems")],
            &mut no_abort(),
        )
        .await
        .unwrap();

        assert_eq!(
            fetched.paths,
            vec![valid_nix_store_path_for_repository_name("systems")]
        );
        assert_eq!(reporter.input_requests().len(), 1);
    }

    #[tokio::test]
    async fn an_abort_stops_the_fetch() {
        let reporter = RecordingJobReporter::new();
        let (tx, mut rx) = AbortSignal::channel();
        tx.send(true).unwrap();
        let err = fetch_with(
            &reporter,
            &FakeWorkerStore::default(),
            vec![input("a", "a")],
            &mut rx,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<WorkerError>(),
                Some(WorkerError::FlakeInputFetchAborted)
            ),
            "{err}"
        );
    }
}
