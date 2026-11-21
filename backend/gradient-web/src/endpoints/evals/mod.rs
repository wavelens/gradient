/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod artefacts;
pub mod log;
mod progress;
pub mod query;
mod refusal;
pub mod report;
mod shared_build_reuse_after_evaluation_abort;
pub mod summary;
pub mod task_run_history;
pub mod types;

pub use self::actions::*;
pub use self::artefacts::*;
pub use self::log::*;
pub(crate) use self::progress::{live_progress, live_progress_status};
pub use self::query::*;
pub use self::report::*;
pub use self::summary::*;
pub use self::types::*;

use crate::access::{
    Caller, ProjectItemAccessRequirement, ProjectItemLocation, can_access_project_resource,
};
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::lookup::require;
use crate::resource::Resource;
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_types::*;

pub(super) struct EvalAccessContext {
    pub evaluation: MEvaluation,
    pub project_id: ProjectId,
    pub project_name: String,
    pub task_name: Option<String>,
    pub task_display_name: Option<String>,
}

impl EvalAccessContext {
    pub(super) async fn load_unguarded(db: &WebDb, evaluation_id: EvaluationId) -> WebResult<Self> {
        Ok(Self::with_project(db, evaluation_id).await?.0)
    }

    pub(super) async fn load(
        db: &WebDb,
        evaluation_id: EvaluationId,
        caller: Caller<'_>,
        api_key: Option<&ApiKeyContext>,
        requirement: ProjectItemAccessRequirement,
    ) -> WebResult<Self> {
        let (ctx, project) = Self::with_project(db, evaluation_id).await?;
        let resource = ProjectItemLocation::from_evaluation(&ctx.evaluation)?;
        can_access_project_resource(
            db,
            caller,
            api_key,
            &project,
            resource,
            requirement,
            Resource::Evaluation,
        )
        .await?;
        Ok(ctx)
    }

    async fn with_project(db: &WebDb, evaluation_id: EvaluationId) -> WebResult<(Self, MProject)> {
        let evaluation = require::<EEvaluation>(db, evaluation_id).await?;

        let task_id = evaluation.task.ok_or_else(|| {
            tracing::warn!(%evaluation_id, "evaluation has no task");
            WebError::data_inconsistency(Resource::Evaluation)
        })?;
        let task = by_id::<ETask, _>(db, task_id).await?.ok_or_else(|| {
            tracing::warn!(
                %task_id,
                %evaluation_id,
                "Task not found for evaluation",
            );
            WebError::data_inconsistency(Resource::Evaluation)
        })?;
        let project_id = task.project;

        let project = by_id::<EProject, _>(db, project_id).await?.ok_or_else(|| {
            tracing::warn!(%project_id, "Project not found");
            WebError::data_inconsistency(Resource::Project)
        })?;

        let ctx = Self {
            evaluation,
            project_id,
            project_name: project.name.clone(),
            task_name: Some(task.name),
            task_display_name: Some(task.display_name),
        };
        Ok((ctx, project))
    }
}
