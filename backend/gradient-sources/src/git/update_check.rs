/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::context::TaskGitContext;
use super::remote::{BranchHead, ls_remote_head};
use crate::SourceError;
use gradient_db::evaluations::branch::newest_on_branch;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
use tracing::{debug, info, instrument, warn};

fn database_error(e: sea_orm::DbErr) -> SourceError {
    SourceError::Database {
        reason: e.to_string(),
    }
}

impl TaskGitContext<'_> {
    #[instrument(skip(self), fields(task_id = %self.task.id, task_name = %self.task.name))]
    pub(super) async fn check_for_updates(
        &self,
        branch: Option<&str>,
    ) -> Result<(bool, BranchHead), SourceError> {
        debug!("Checking for updates on task");

        let head = self.remote_head(branch).await?;
        debug!(remote_hash = %vec_to_hex(&head.commit), branch = %head.branch, "Retrieved remote hash");

        let newest = newest_on_branch(&self.ctx.worker_db, self.task.id, &head.branch)
            .await
            .map_err(database_error)?;
        let head_evaluated = match &newest {
            Some(evaluation) => self.commit_hash(evaluation).await? == Some(head.commit.clone()),
            None => false,
        };

        if self.task.force_evaluation {
            if head_evaluated && newest.is_some_and(|e| e.status.is_active()) {
                debug!("Evaluation of the remote head in progress, skipping forced re-eval");
                return Ok((false, head));
            }
            info!("Force evaluation enabled, updating task");
            self.clear_force_evaluation().await;
            return Ok((true, head));
        }

        if head_evaluated {
            debug!("Remote hash matches the newest evaluation of the branch, no update needed");
            return Ok((false, head));
        }

        info!("Remote hash differs from the newest evaluation of the branch, update needed");
        Ok((true, head))
    }

    pub(super) async fn remote_head(
        &self,
        branch: Option<&str>,
    ) -> Result<BranchHead, SourceError> {
        let url = self.task.repository.clone();
        let private_key = self.private_key.clone();
        let branch_owned = branch.map(|b| b.to_owned());

        tokio::task::spawn_blocking(move || {
            ls_remote_head(&url, private_key.as_deref(), branch_owned.as_deref())
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        })?
    }

    async fn commit_hash(&self, evaluation: &MEvaluation) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(ECommit::find_by_id(evaluation.commit)
            .one(&self.ctx.worker_db)
            .await
            .map_err(database_error)?
            .map(|commit| commit.hash))
    }

    async fn clear_force_evaluation(&self) {
        if let Err(e) = ETask::update_many()
            .col_expr(CTask::ForceEvaluation, Expr::value(false))
            .filter(CTask::Id.eq(self.task.id))
            .exec(&self.ctx.worker_db)
            .await
        {
            warn!(error = %e, "failed to clear force_evaluation flag");
        }
    }
}
