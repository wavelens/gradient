/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::checkout::Origin;
use super::commit_info::head_refspec;
use super::context::{TaskGitContext, save_observed_project_ssh_host_keys};
use super::git_credentials::{
    GitOperationCredentials, run_blocking_libgit2_operation_with_git_credentials,
    run_with_replaced_thread_local_git_credentials,
};
use super::git_network_limits::BlockingGitFetchOperationLimits;
use crate::SourceError;
use futures::FutureExt;
use git2::{Oid, Repository};
use gradient_types::RepositoryUrl;
use gradient_types::ids::ProjectId;
use gradient_util::shutdown::{CancellationToken, Shutdown};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

const FINISHED_COMPARISON_RESULT_STORAGE_TIME: Duration = Duration::from_secs(10 * 60);
// Tree diffs compare the tip of the branch with the head commit. A new tip can add paths, so stored lists for branch bases expire after a short time.
const FINISHED_CHANGED_FILE_LIST_STORAGE_TIME_FOR_BRANCH_NAME_BASE: Duration =
    Duration::from_secs(2 * 60);
const MAX_STORED_COMPARISON_RESULTS: usize = 1024;
const MAX_STORED_CHANGED_FILE_PATHS_PER_COMPARISON: usize = 10_000;

type ChangedFileListAndBaseCommitCacheForProjectAndRepository =
    ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<
        (ProjectId, String),
        (ProjectId, ChangedFileComparison),
        ChangedFilePathsAndBaseCommit,
        SourceError,
    >;

static CHANGED_PATH_FILTERS: LazyLock<
    Arc<ChangedFileListAndBaseCommitCacheForProjectAndRepository>,
> = LazyLock::new(|| {
    Arc::new(
        ChangedFileComparisonCacheWithOneRunPerProjectAndRepository::new(
            changed_files_cache_duration_for_comparison_base,
            changed_file_list_has_at_most_max_paths_and_can_be_kept_in_comparison_cache,
        ),
    )
});

pub(crate) fn changed_file_list_has_at_most_max_paths_and_can_be_kept_in_comparison_cache(
    list: &ChangedFilePathsAndBaseCommit,
) -> bool {
    list.changed_file_paths.len() <= MAX_STORED_CHANGED_FILE_PATHS_PER_COMPARISON
}

pub(crate) fn changed_files_cache_duration_for_comparison_base(
    (_, comparison): &(ProjectId, ChangedFileComparison),
) -> Duration {
    match comparison.base {
        ComparisonBaseCommit::CommitHashBytes(_) => FINISHED_COMPARISON_RESULT_STORAGE_TIME,
        ComparisonBaseCommit::BranchName(_) => {
            FINISHED_CHANGED_FILE_LIST_STORAGE_TIME_FOR_BRANCH_NAME_BASE
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFilePathsAndBaseCommit {
    pub(crate) changed_file_paths: Vec<String>,
    pub(crate) comparison_base_commit_id_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ComparisonBaseCommit {
    CommitHashBytes(Vec<u8>),
    BranchName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFileComparison {
    pub(crate) task_repository_url: RepositoryUrl,
    pub(crate) base: ComparisonBaseCommit,
    pub(crate) head_commit_repository_url: RepositoryUrl,
    pub(crate) head: Vec<u8>,
}

impl Hash for ChangedFileComparison {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.task_repository_url.git_connection_url().hash(state);
        self.base.hash(state);
        self.head_commit_repository_url
            .git_connection_url()
            .hash(state);
        self.head.hash(state);
    }
}

type ChangedPathsComparisonResultWatchReceiver<Output, Error> =
    watch::Receiver<Option<Result<Output, Error>>>;
type RunningComparisonAndResultReceiverByProjectAndRepository<Key, Comparison, Output, Error> =
    HashMap<
        Key,
        (
            Comparison,
            ChangedPathsComparisonResultWatchReceiver<Output, Error>,
        ),
    >;
type ExpiringComparisonResultMap<Comparison, Output> = HashMap<Comparison, (Instant, Output)>;
type RunningComparisonsWithReceiversAndFinishedResultsWithStoredTime<
    Key,
    Comparison,
    Output,
    Error,
> = (
    RunningComparisonAndResultReceiverByProjectAndRepository<Key, Comparison, Output, Error>,
    ExpiringComparisonResultMap<Comparison, Output>,
);

pub(crate) struct ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<
    Key,
    Comparison,
    Output,
    Error,
> {
    running_comparisons_and_finished_results_by_key: Mutex<
        RunningComparisonsWithReceiversAndFinishedResultsWithStoredTime<
            Key,
            Comparison,
            Output,
            Error,
        >,
    >,
    finished_result_retention_duration_for_comparison: fn(&Comparison) -> Duration,
    finished_result_is_small_enough_to_keep: fn(&Output) -> bool,
}

impl<Key, Comparison, Output, Error>
    ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<Key, Comparison, Output, Error>
{
    pub(crate) fn new(
        finished_result_retention_duration_for_comparison: fn(&Comparison) -> Duration,
        finished_result_is_small_enough_to_keep: fn(&Output) -> bool,
    ) -> Self {
        Self {
            running_comparisons_and_finished_results_by_key: Mutex::new((
                HashMap::new(),
                HashMap::new(),
            )),
            finished_result_retention_duration_for_comparison,
            finished_result_is_small_enough_to_keep,
        }
    }
}

impl<Key, Comparison, Output, Error> Default
    for ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<Key, Comparison, Output, Error>
{
    fn default() -> Self {
        Self::new(|_| FINISHED_COMPARISON_RESULT_STORAGE_TIME, |_| true)
    }
}

impl<Key, Comparison, Output, Error>
    ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<Key, Comparison, Output, Error>
where
    Key: Clone + Eq + Hash + Send + 'static,
    Comparison: Clone + Eq + Hash + Send + 'static,
    Output: Clone + Send + Sync + 'static,
    Error: Clone + Send + Sync + 'static,
{
    pub(crate) async fn get_or_wait_for_or_start_comparison_result<RunFuture>(
        self: &Arc<Self>,
        shutdown: &Shutdown,
        key: Key,
        comparison: Comparison,
        start_run: impl FnOnce() -> RunFuture,
    ) -> Option<Result<Output, Error>>
    where
        RunFuture: Future<Output = Result<Output, Error>> + Send + 'static,
    {
        let (sender, receiver) = loop {
            let (receiver, runs_same_comparison) = {
                let mut guard = self
                    .running_comparisons_and_finished_results_by_key
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let (running, finished) = &mut *guard;
                if let Some((stored_at, output)) = finished.get(&comparison)
                    && stored_at.elapsed()
                        < (self.finished_result_retention_duration_for_comparison)(&comparison)
                {
                    return Some(Ok(output.clone()));
                }
                match running.get(&key) {
                    Some((running_comparison, receiver)) => {
                        (receiver.clone(), *running_comparison == comparison)
                    }
                    None => {
                        let (sender, receiver) = watch::channel(None);
                        running.insert(key.clone(), (comparison.clone(), receiver.clone()));
                        break (sender, receiver);
                    }
                }
            };
            let output = wait_for_comparison_result_or_none(receiver).await;
            if runs_same_comparison {
                return output;
            }
        };
        shutdown.spawn(
            Arc::clone(self).complete_comparison_run_store_success_and_notify_waiters(
                key,
                comparison,
                start_run(),
                sender,
                shutdown.token(),
            ),
        );
        wait_for_comparison_result_or_none(receiver).await
    }

    async fn complete_comparison_run_store_success_and_notify_waiters(
        self: Arc<Self>,
        key: Key,
        comparison: Comparison,
        run: impl Future<Output = Result<Output, Error>>,
        sender: watch::Sender<Option<Result<Output, Error>>>,
        shutdown_cancellation_token: CancellationToken,
    ) {
        let output = tokio::select! {
            output = AssertUnwindSafe(run).catch_unwind() => output.ok(),
            () = shutdown_cancellation_token.cancelled() => None,
        };
        {
            let mut guard = self
                .running_comparisons_and_finished_results_by_key
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let (running, finished) = &mut *guard;
            running.remove(&key);
            if let Some(Ok(value)) = &output
                && (self.finished_result_is_small_enough_to_keep)(value)
            {
                finished.retain(|stored, (stored_at, _)| {
                    stored_at.elapsed()
                        < (self.finished_result_retention_duration_for_comparison)(stored)
                });
                if finished.len() >= MAX_STORED_COMPARISON_RESULTS
                    && let Some(oldest) = finished
                        .iter()
                        .min_by_key(|(_, (stored_at, _))| *stored_at)
                        .map(|(stored, _)| stored.clone())
                {
                    finished.remove(&oldest);
                }
                finished.insert(comparison, (Instant::now(), value.clone()));
            }
        }
        if let Some(output) = output {
            sender.send_replace(Some(output));
        }
    }
}

async fn wait_for_comparison_result_or_none<Output: Clone, Error: Clone>(
    mut receiver: ChangedPathsComparisonResultWatchReceiver<Output, Error>,
) -> Option<Result<Output, Error>> {
    receiver.wait_for(Option::is_some).await.ok()?.clone()
}

pub(super) fn project_git_credentials_for_pull_request_head_on_same_host_and_port(
    project_repository: &RepositoryUrl,
    head_repository: &RepositoryUrl,
    credentials: &GitOperationCredentials,
) -> GitOperationCredentials {
    if head_repository.git_server_host_name() == project_repository.git_server_host_name()
        && head_repository.git_server_port() == project_repository.git_server_port()
    {
        credentials.clone()
    } else {
        GitOperationCredentials::default()
    }
}

pub(crate) fn fetch_base_and_head_into_temp_repo_and_get_changed_paths(
    comparison: &ChangedFileComparison,
    credentials: &GitOperationCredentials,
) -> Result<ChangedFilePathsAndBaseCommit, SourceError> {
    let head_fetch_credentials =
        project_git_credentials_for_pull_request_head_on_same_host_and_port(
            &comparison.task_repository_url,
            &comparison.head_commit_repository_url,
            credentials,
        );
    run_blocking_libgit2_operation_with_git_credentials(
        credentials,
        BlockingGitFetchOperationLimits::path_filter_fetch_operation_limits(),
        || {
            let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
                reason: e.to_string(),
            })?;
            changed_paths(temp_dir.path(), comparison, head_fetch_credentials).map_err(|e| {
                SourceError::GitCommandFailed {
                    stderr: e.message().to_string(),
                }
            })
        },
    )
}

fn changed_paths(
    dir: &Path,
    comparison: &ChangedFileComparison,
    head_fetch_credentials: GitOperationCredentials,
) -> Result<ChangedFilePathsAndBaseCommit, git2::Error> {
    let repo = Repository::init_bare(dir)?;

    let base = fetch_base(
        &repo,
        comparison.task_repository_url.git_connection_url(),
        &comparison.base,
    )?;
    let head = Oid::from_bytes(&comparison.head)?;
    run_with_replaced_thread_local_git_credentials(head_fetch_credentials, || {
        Origin::new(
            &repo,
            comparison.head_commit_repository_url.git_connection_url(),
        )?
        .fetch_commit(head)
    })?;

    let base_tree = repo.find_commit(base)?.tree()?;
    let head_tree = repo.find_commit(head)?.tree()?;
    let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?;
    let paths: BTreeSet<String> = diff
        .deltas()
        .flat_map(|delta| [delta.old_file().path(), delta.new_file().path()])
        .flatten()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    Ok(ChangedFilePathsAndBaseCommit {
        changed_file_paths: paths.into_iter().collect(),
        comparison_base_commit_id_bytes: base.as_bytes().to_vec(),
    })
}

fn fetch_base(
    repo: &Repository,
    url: &str,
    base: &ComparisonBaseCommit,
) -> Result<Oid, git2::Error> {
    let mut origin = Origin::new(repo, url)?;
    match base {
        ComparisonBaseCommit::CommitHashBytes(hash) => {
            let oid = Oid::from_bytes(hash)?;
            origin.fetch_commit(oid)?;
            Ok(oid)
        }
        ComparisonBaseCommit::BranchName(branch) => {
            origin.fetch(&head_refspec(Some(branch)), true)?;
            Ok(repo.revparse_single("FETCH_HEAD")?.peel_to_commit()?.id())
        }
    }
}

impl TaskGitContext<'_> {
    pub(super) async fn changed_paths(
        &self,
        base: ComparisonBaseCommit,
        head: Vec<u8>,
        head_repository: Option<&RepositoryUrl>,
    ) -> Result<Vec<String>, SourceError> {
        let comparison = ChangedFileComparison {
            task_repository_url: self.repository.clone(),
            base,
            head_commit_repository_url: head_repository.unwrap_or(&self.repository).clone(),
            head,
        };
        let task_repository_git_connection_url = self.repository.git_connection_url().to_owned();
        let (credentials, worker_db, project) = (
            self.credentials.clone(),
            self.ctx.worker_db.clone(),
            self.task.project,
        );
        let blocking_comparison = comparison.clone();
        let run = move || async move {
            let blocking_credentials = credentials.clone();
            let changed = tokio::task::spawn_blocking(move || {
                fetch_base_and_head_into_temp_repo_and_get_changed_paths(
                    &blocking_comparison,
                    &blocking_credentials,
                )
            })
            .await
            .map_err(|e| SourceError::GitExecution {
                error: e.to_string(),
            });
            save_observed_project_ssh_host_keys(
                &worker_db,
                project,
                credentials.ssh_git_private_key_and_known_hosts.as_ref(),
            )
            .await;
            changed?
        };
        CHANGED_PATH_FILTERS
            .get_or_wait_for_or_start_comparison_result(
                &self.ctx.shutdown,
                (project, task_repository_git_connection_url),
                (project, comparison),
                run,
            )
            .await
            .unwrap_or_else(|| {
                Err(SourceError::GitExecution {
                    error: "shared changed path listing ended without a result".to_owned(),
                })
            })
            .map(|changed| changed.changed_file_paths)
    }
}
