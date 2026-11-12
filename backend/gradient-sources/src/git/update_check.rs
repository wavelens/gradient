/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::context::{TaskGitContext, save_observed_project_ssh_host_keys};
use super::remote::{AdvertisedRef, BranchHead, branch_head, list_remote_refs, tag_commit};
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
        let branch = branch.map(str::to_owned);
        self.select_remote_ref(move |refs| branch_head(refs, branch.as_deref()))
            .await
    }

    pub(super) async fn remote_tag_commit(&self, tag: &str) -> Result<Vec<u8>, SourceError> {
        let tag = tag.to_owned();
        self.select_remote_ref(move |refs| tag_commit(refs, &tag))
            .await
    }

    async fn select_remote_ref<T, F>(&self, select: F) -> Result<T, SourceError>
    where
        T: Send + 'static,
        F: FnOnce(&[AdvertisedRef]) -> Result<T, SourceError> + Send + 'static,
    {
        let url = self.task.repository.clone();
        let ssh = self.ssh.clone();

        let selected =
            tokio::task::spawn_blocking(move || select(&list_remote_refs(&url, ssh.as_ref())?))
                .await
                .map_err(|e| SourceError::GitExecution {
                    error: e.to_string(),
                });
        save_observed_project_ssh_host_keys(
            &self.ctx.worker_db,
            self.task.project,
            self.ssh.as_ref(),
        )
        .await;
        selected?
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
