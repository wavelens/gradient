/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod log;
pub mod query;

pub use self::actions::post_runner_job_abort;
pub use self::log::get_runner_job_log;
pub use self::query::{
    RunnerJobAttemptView, RunnerJobPhaseView, RunnerJobResponse, RunnerJobSummary, get_runner_job,
    list_evaluation_runner_jobs,
};

use crate::access::is_project_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::refusal::{Refusal, RefusalStatus};
use crate::resource::Resource;
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_db::runner_jobs::reads::{self, RunnerJobDetail};
use gradient_types::*;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunnerJobRefusal {
    #[error("the runner job has already finished")]
    Finished,
}

impl Refusal for RunnerJobRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::Finished => RefusalStatus::Conflict,
        }
    }
}

pub(super) struct RunnerJobAccessContext {
    pub detail: RunnerJobDetail,
    pub task: MTask,
    pub project: MProject,
}

impl RunnerJobAccessContext {
    pub(super) async fn load_unguarded(db: &WebDb, job: RunnerJobId) -> WebResult<Self> {
        let detail = reads::one(db, job)
            .await?
            .ok_or_else(|| WebError::not_found(Resource::RunnerJob))?;
        let task_id = detail.task.ok_or_else(|| {
            tracing::warn!(runner_job = %job, "runner job evaluation has no task");
            WebError::data_inconsistency(Resource::RunnerJob)
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
        job: RunnerJobId,
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
            return Err(WebError::not_found(Resource::RunnerJob));
        }
        Ok(ctx)
    }
}
