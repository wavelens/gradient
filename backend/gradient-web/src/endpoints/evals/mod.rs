/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod artefacts;
pub mod log;
pub mod query;
pub mod report;
pub mod types;

pub use self::actions::*;
pub use self::artefacts::*;
pub use self::log::*;
pub use self::query::*;
pub use self::report::*;
pub use self::types::*;

use crate::access::is_project_member;
use crate::authorization::ApiKeyContext;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;

/// Resolved access context for an evaluation.
///
/// Loaded once per request: fetches the evaluation row, resolves the owning
/// project through the task, and enforces the access check. Returns
/// `not_found("Evaluation")` on any failure so callers cannot distinguish
/// missing from forbidden.
pub(super) struct EvalAccessContext {
    pub evaluation: MEvaluation,
    pub project_id: ProjectId,
    pub task_name: Option<String>,
    pub task_display_name: Option<String>,
}

impl EvalAccessContext {
    pub(super) async fn load(
        state: &Arc<ServerState>,
        evaluation_id: EvaluationId,
        maybe_user: &Option<MUser>,
        api_key: Option<&ApiKeyContext>,
    ) -> WebResult<Self> {
        let evaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&state.web_db)
            .await?
            .or_not_found("Evaluation")?;

        let task_id = evaluation.task.ok_or_else(|| {
            tracing::warn!(%evaluation_id, "evaluation has no task");
            WebError::data_inconsistency("Evaluation")
        })?;
        let task = ETask::find_by_id(task_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(
                    %task_id,
                    %evaluation_id,
                    "Task not found for evaluation",
                );
                WebError::data_inconsistency("Evaluation")
            })?;
        let project_id = task.project;
        let task_name = Some(task.name);
        let task_display_name = Some(task.display_name);

        let project = EProject::find_by_id(project_id)
            .one(&state.web_db)
            .await?
            .ok_or_else(|| {
                tracing::warn!(%project_id, "Project not found");
                WebError::data_inconsistency("Project")
            })?;

        let can_access = if project.public {
            true
        } else {
            match maybe_user {
                Some(user) => is_project_member(state, user.id, project.id, api_key).await?,
                None => false,
            }
        };
        if !can_access {
            return Err(WebError::not_found("Evaluation"));
        }

        Ok(Self {
            evaluation,
            project_id,
            task_name,
            task_display_name,
        })
    }
}
