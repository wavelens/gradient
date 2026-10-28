/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod apply;
pub mod branch;

use crate::actions::send::git_host_status::build_reporter_for_integration;
use crate::actions::send::open_pr::configured_commit_ident;
use crate::context::CiContext;
use anyhow::{Context, anyhow};
use gradient_db::WorkerDb;
use gradient_db::lookup::by_id;
use gradient_git_host::reporter::{CiReporter, parse_owner_repo};
use gradient_git_host::{BranchCommit, CommitFile, CommitIdent};
use gradient_types::input::{check_repository_url_is_ssh, vec_to_hex};
use gradient_types::{
    ActionConfig, ActionType, CTaskAction, ECommit, EEvaluation, EExecJob, EProject, ETaskAction,
    ExecJobId, IntegrationId, MCommit, MTask, TaskId,
};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub use apply::{patch_paths, patched_files};
pub use branch::default_branch_name;

pub const MAX_PULL_REQUEST_PATCH_BYTES: usize = 8 * 1024 * 1024;

const COMMIT_HASH_BYTES: usize = 20;

#[derive(Debug, Clone)]
pub struct PatchPullRequest {
    pub task: MTask,
    pub integration: IntegrationId,
    pub start_commit: String,
    pub patch: Vec<u8>,
    pub title: String,
    pub body: String,
    pub branch: String,
    pub base: Option<String>,
    pub job: Option<ExecJobId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedPullRequest {
    pub url: String,
    pub number: i64,
    pub branch: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PullRequestError {
    #[error("the task has no open_pr action naming a Git host integration")]
    NoIntegration,
    #[error("the job's evaluation has no git commit to apply the patch to")]
    NoCommit,
    #[error("the patch deletes {0}; deleting files is not supported")]
    DeletesFile(String),
    #[error("the patch does not apply: {0}")]
    DoesNotApply(String),
    #[error("the patch changes nothing")]
    Empty,
    #[error("the Git host refused the pull request: {0:#}")]
    Forge(anyhow::Error),
}

pub async fn open_from_patch(
    ctx: &CiContext,
    request: PatchPullRequest,
) -> Result<OpenedPullRequest, PullRequestError> {
    let (owner, repo) = parse_owner_repo(&request.task.repository).ok_or_else(|| {
        PullRequestError::Forge(anyhow!(
            "cannot read owner and repository from {}",
            request.task.repository
        ))
    })?;
    let files = files_of_patch(ctx, &request).await?;
    let reporter = build_reporter_for_integration(ctx, request.integration)
        .await
        .map_err(PullRequestError::Forge)?;
    let author = configured_commit_ident(
        &ctx.db.config.pull_requests.commit_name,
        &ctx.db.config.pull_requests.commit_email,
    );
    let opened =
        open_with_reporter(reporter.as_ref(), &owner, &repo, files, &request, author).await?;
    record_for_job(ctx, &request, &opened).await?;
    Ok(opened)
}

pub(crate) async fn open_with_reporter(
    reporter: &dyn CiReporter,
    owner: &str,
    repo: &str,
    files: Vec<CommitFile>,
    request: &PatchPullRequest,
    author: Option<CommitIdent>,
) -> Result<OpenedPullRequest, PullRequestError> {
    let base = match &request.base {
        Some(base) => base.clone(),
        None => reporter
            .default_branch(owner, repo)
            .await
            .map_err(PullRequestError::Forge)?,
    };
    let commit = BranchCommit {
        message: request.title.clone(),
        author,
        files,
    };
    reporter
        .upsert_branch(owner, repo, &request.branch, &base, &commit)
        .await
        .map_err(PullRequestError::Forge)?;
    let pr = reporter
        .open_or_update_pr(
            owner,
            repo,
            &request.branch,
            &base,
            &request.title,
            &request.body,
        )
        .await
        .map_err(PullRequestError::Forge)?;
    Ok(OpenedPullRequest {
        url: pr.url.unwrap_or_default(),
        number: pr.number,
        branch: request.branch.clone(),
    })
}

pub async fn pull_request_integration(
    db: &WorkerDb,
    task: TaskId,
) -> Result<Option<IntegrationId>, DbErr> {
    let actions = ETaskAction::find()
        .filter(CTaskAction::Task.eq(task))
        .filter(CTaskAction::Active.eq(true))
        .filter(CTaskAction::ActionType.eq(ActionType::OpenPr))
        .order_by_asc(CTaskAction::CreatedAt)
        .all(db)
        .await?;
    Ok(actions.into_iter().find_map(|action| {
        match serde_json::from_value::<ActionConfig>(action.config).ok()? {
            ActionConfig::OpenPr { integration_id, .. } => Some(integration_id),
            _ => None,
        }
    }))
}

pub async fn evaluation_commit_of_job(
    db: &WorkerDb,
    job: ExecJobId,
) -> Result<String, PullRequestError> {
    let commit = evaluation_commit_row(db, job)
        .await
        .context("load the job's evaluation commit")
        .map_err(PullRequestError::Forge)?
        .filter(|commit| commit.hash.len() == COMMIT_HASH_BYTES)
        .ok_or(PullRequestError::NoCommit)?;
    Ok(vec_to_hex(&commit.hash))
}

async fn evaluation_commit_row(db: &WorkerDb, job: ExecJobId) -> Result<Option<MCommit>, DbErr> {
    let Some(job) = by_id::<EExecJob, _>(db, job).await? else {
        return Ok(None);
    };
    let Some(evaluation) = by_id::<EEvaluation, _>(db, job.evaluation).await? else {
        return Ok(None);
    };
    by_id::<ECommit, _>(db, evaluation.commit).await
}

async fn files_of_patch(
    ctx: &CiContext,
    request: &PatchPullRequest,
) -> Result<Vec<CommitFile>, PullRequestError> {
    let ssh_key = repository_ssh_key(ctx, &request.task).await?;
    let repository = request.task.repository.clone();
    let start_commit = request.start_commit.clone();
    let patch = request.patch.clone();
    let files = tokio::task::spawn_blocking(move || {
        let checkout =
            gradient_sources::checkout_commit(&repository, &start_commit, ssh_key.as_deref())
                .map_err(|e| PullRequestError::DoesNotApply(format!("{e:#}")))?;
        patched_files(checkout.path(), &patch)
    })
    .await
    .map_err(|e| PullRequestError::Forge(anyhow!("apply the patch: {e}")))??;
    if files.is_empty() {
        return Err(PullRequestError::Empty);
    }
    Ok(files)
}

async fn repository_ssh_key(
    ctx: &CiContext,
    task: &MTask,
) -> Result<Option<String>, PullRequestError> {
    if !check_repository_url_is_ssh(&task.repository) {
        return Ok(None);
    }
    let project = by_id::<EProject, _>(&ctx.db.worker_db, task.project)
        .await
        .context("load the task's project")
        .map_err(PullRequestError::Forge)?
        .ok_or_else(|| PullRequestError::Forge(anyhow!("the task's project is gone")))?;
    let (private_key, _) = gradient_sources::decrypt_ssh_private_key(
        &ctx.db.config.secrets.crypt_file,
        project,
        &ctx.db.config.server.serve_url,
    )
    .map_err(|e| PullRequestError::Forge(anyhow!("decrypt the project's SSH key: {e}")))?;
    Ok(Some(private_key))
}

async fn record_for_job(
    ctx: &CiContext,
    request: &PatchPullRequest,
    opened: &OpenedPullRequest,
) -> Result<(), PullRequestError> {
    let Some(job) = request.job else {
        return Ok(());
    };
    gradient_db::exec_jobs::pull_requests::record(
        &ctx.db.worker_db,
        job,
        request.integration,
        &opened.url,
        &opened.branch,
        opened.number,
    )
    .await
    .context("record the job's pull request")
    .map_err(PullRequestError::Forge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use gradient_git_host::PrRef;
    use gradient_git_host::reporter::CiReport;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct RecordingReporter {
        branches: Mutex<Vec<String>>,
        opened: Mutex<Vec<String>>,
    }

    impl RecordingReporter {
        fn opened(&self) -> Vec<String> {
            self.opened.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CiReporter for RecordingReporter {
        async fn report(&self, _report: &CiReport) -> anyhow::Result<Option<i64>> {
            Ok(None)
        }

        async fn upsert_branch(
            &self,
            _owner: &str,
            _repo: &str,
            branch: &str,
            _base: &str,
            _commit: &BranchCommit,
        ) -> anyhow::Result<String> {
            self.branches.lock().unwrap().push(branch.to_owned());
            Ok("0".repeat(40))
        }

        async fn open_or_update_pr(
            &self,
            _owner: &str,
            _repo: &str,
            head: &str,
            _base: &str,
            _title: &str,
            _body: &str,
        ) -> anyhow::Result<PrRef> {
            let mut opened = self.opened.lock().unwrap();
            let number = match opened.iter().position(|branch| branch == head) {
                Some(index) => index,
                None => {
                    opened.push(head.to_owned());
                    opened.len() - 1
                }
            };
            Ok(PrRef {
                number: i64::try_from(number).unwrap() + 1,
                url: Some(format!("https://git.example/pr/{}", number + 1)),
            })
        }

        async fn default_branch(&self, _owner: &str, _repo: &str) -> anyhow::Result<String> {
            Ok("main".into())
        }
    }

    fn files(paths: &[&str]) -> Vec<CommitFile> {
        paths
            .iter()
            .map(|path| CommitFile {
                path: (*path).into(),
                contents: b"x".to_vec(),
            })
            .collect()
    }

    fn request(paths: &[&str], branch: Option<&str>) -> PatchPullRequest {
        let touched: Vec<String> = paths.iter().map(|path| (*path).into()).collect();
        PatchPullRequest {
            task: MTask::default(),
            integration: IntegrationId::nil(),
            start_commit: "0".repeat(40),
            patch: Vec::new(),
            title: "update".into(),
            body: String::new(),
            branch: branch.map_or_else(
                || default_branch_name(Some(ExecJobId::nil()), &touched),
                str::to_owned,
            ),
            base: None,
            job: Some(ExecJobId::nil()),
        }
    }

    #[tokio::test]
    async fn two_patches_with_different_paths_open_two_pull_requests() {
        let reporter = RecordingReporter::default();
        let lock = request(&["flake.lock"], None);
        let cargo = request(&["Cargo.lock"], None);

        let first = open_with_reporter(
            &reporter,
            "acme",
            "infra",
            files(&["flake.lock"]),
            &lock,
            None,
        )
        .await
        .unwrap();
        let second = open_with_reporter(
            &reporter,
            "acme",
            "infra",
            files(&["Cargo.lock"]),
            &cargo,
            None,
        )
        .await
        .unwrap();

        assert_ne!(first.branch, second.branch);
        assert_ne!(first.number, second.number);
        assert_eq!(reporter.opened().len(), 2);
    }

    #[test]
    fn the_default_branch_follows_the_touched_paths() {
        let job = ExecJobId::nil();
        assert_eq!(
            default_branch_name(Some(job), &["b".into(), "a".into()]),
            default_branch_name(Some(job), &["a".into(), "b".into()])
        );
        assert_ne!(
            default_branch_name(Some(job), &["a".into()]),
            default_branch_name(Some(job), &["b".into()])
        );
        assert!(default_branch_name(Some(job), &["a".into()]).starts_with("gradient/00000000/"));
    }
}
