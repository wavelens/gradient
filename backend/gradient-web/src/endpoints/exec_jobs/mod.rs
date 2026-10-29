/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod log;
pub mod outputs;
pub mod pull_requests;
pub mod query;
pub mod terminal;

pub use self::actions::{
    delete_exec_job_keep_vm_running, post_exec_job_abort, post_exec_job_keep_vm_running,
    post_exec_job_run,
};
pub use self::log::get_exec_job_log;
pub use self::outputs::{ExecJobOutputView, get_exec_job_output, list_exec_job_outputs};
pub use self::pull_requests::{ExecJobPullRequestView, list_exec_job_pull_requests};
pub use self::query::{
    ExecJobAttemptView, ExecJobNode, ExecJobPhaseView, ExecJobResponse, ExecJobSummary,
    ExecJobTokenView, StartedEvaluationView, get_exec_job, list_evaluation_exec_jobs,
};

use crate::access::is_project_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::refusal::{Refusal, RefusalStatus};
use crate::resource::Resource;
use gradient_db::WebDb;
use gradient_db::exec_jobs::reads::{self, ExecJobDetail, ExecNodeDetail};
use gradient_db::lookup::by_id;
use gradient_types::*;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecJobRefusal {
    #[error("the exec job has already finished")]
    Finished,
    #[error("no node {0} in this job")]
    NoSuchNode(u32),
}

impl Refusal for ExecJobRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::Finished => RefusalStatus::Conflict,
            Self::NoSuchNode(_) => RefusalStatus::NotFound,
        }
    }
}

pub(super) struct ExecJobAccessContext {
    pub detail: ExecJobDetail,
    pub task: MTask,
    pub project: MProject,
}

impl ExecJobAccessContext {
    pub(super) fn node(&self, index: Option<u32>) -> WebResult<&ExecNodeDetail> {
        let index = index.unwrap_or(0);
        self.detail
            .node(index)
            .ok_or_else(|| ExecJobRefusal::NoSuchNode(index).into())
    }

    pub(super) async fn load_unguarded(db: &WebDb, job: ExecJobId) -> WebResult<Self> {
        let detail = reads::one(db, job)
            .await?
            .ok_or_else(|| WebError::not_found(Resource::ExecJob))?;
        let task_id = detail.task.ok_or_else(|| {
            tracing::warn!(exec_job = %job, "exec job evaluation has no task");
            WebError::data_inconsistency(Resource::ExecJob)
        })?;
        let task = by_id::<ETask, _>(db, task_id)
            .await?
            .ok_or_else(|| WebError::data_inconsistency(Resource::Task))?;
        let project = by_id::<EProject, _>(db, task.project)
            .await?
            .ok_or_else(|| WebError::data_inconsistency(Resource::Project))?;
        Ok(Self {
            detail,
            task,
            project,
        })
    }

    pub(super) async fn load(
        db: &WebDb,
        job: ExecJobId,
        maybe_user: &Option<MUser>,
        api_key: Option<&ApiKeyContext>,
    ) -> WebResult<Self> {
        let ctx = Self::load_unguarded(db, job).await?;
        let readable = ctx.project.public
            || match maybe_user {
                Some(user) => is_project_member(db, user.id, ctx.project.id, api_key).await?,
                None => false,
            };
        if !readable {
            return Err(WebError::not_found(Resource::ExecJob));
        }
        Ok(ctx)
    }
}
