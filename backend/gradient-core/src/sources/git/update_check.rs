/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::context::ProjectGitContext;
use super::remote::ls_remote_head;
use crate::sources::SourceError;
use crate::types::input::vec_to_hex;
use crate::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
use tracing::{debug, info, instrument, warn};

impl ProjectGitContext<'_> {
    /// Check whether there is a new commit on the remote ref.
    ///
    /// `branch = None` polls the remote HEAD (default branch).
    /// `branch = Some("main")` polls `refs/heads/main`.
    ///
    /// Returns `(has_update, remote_hash)`. `has_update` is `false` when the
    /// remote ref matches the last evaluated commit or an evaluation is already
    /// in progress.
    #[instrument(skip(self), fields(project_id = %self.project.id, project_name = %self.project.name))]
    pub(super) async fn check_for_updates(
        &self,
        branch: Option<&str>,
    ) -> Result<(bool, Vec<u8>), SourceError> {
        debug!("Checking for updates on project");

        let url = self.project.repository.clone();
        let ssh_creds = self.ssh_creds.clone();
        let branch_owned = branch.map(|b| b.to_owned());

        let remote_hash = tokio::task::spawn_blocking(move || {
            let branch_ref = branch_owned.as_deref();
            if let Some((private_key, public_key)) = ssh_creds {
                ls_remote_head(&url, Some(&private_key), Some(&public_key), branch_ref)
            } else {
                ls_remote_head(&url, None, None, branch_ref)
            }
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        })??;

        let remote_hash_str = vec_to_hex(&remote_hash);
        debug!(remote_hash = %remote_hash_str, "Retrieved remote hash");

        if self.project.force_evaluation {
            // Never supersede an in-flight evaluation: the one-shot force is
            // already satisfied by whatever is running, and re-triggering would
            // let the concurrency policy abort a build that is still making
            // progress (perpetual re-eval under a fast poll interval).
            if let Some(last_evaluation) = self.project.last_evaluation
                && let Some(evaluation) = EEvaluation::find_by_id(last_evaluation)
                    .one(&self.ctx.worker_db)
                    .await
                    .map_err(|e| SourceError::Database {
                        reason: e.to_string(),
                    })?
                && evaluation.status.is_active()
            {
                debug!(status = ?evaluation.status, "Evaluation already in progress, skipping forced re-eval");
                return Ok((false, remote_hash));
            }
            info!("Force evaluation enabled, updating project");
            // Consume the one-shot flag so subsequent polls fall back to normal
            // commit-change detection instead of forcing on every cycle.
            if let Err(e) = EProject::update_many()
                .col_expr(CProject::ForceEvaluation, Expr::value(false))
                .filter(CProject::Id.eq(self.project.id))
                .exec(&self.ctx.worker_db)
                .await
            {
                warn!(error = %e, "failed to clear force_evaluation flag");
            }
            return Ok((true, remote_hash));
        }

        // A dangling `last_evaluation` (row deleted but pointer stale) is
        // treated as "no previous evaluation, update needed" - same path as
        // a freshly-created project. The pointer self-heals on the next
        // successful trigger.
        if let Some(last_evaluation) = self.project.last_evaluation {
            let evaluation = EEvaluation::find_by_id(last_evaluation)
                .one(&self.ctx.worker_db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?;

            if let Some(evaluation) = evaluation {
                if evaluation.status.is_active() {
                    debug!(status = ?evaluation.status, "Evaluation already in progress, skipping");
                    return Ok((false, remote_hash));
                }

                let commit = ECommit::find_by_id(evaluation.commit)
                    .one(&self.ctx.worker_db)
                    .await
                    .map_err(|e| SourceError::Database {
                        reason: e.to_string(),
                    })?;

                if let Some(commit) = commit {
                    if commit.hash == remote_hash {
                        debug!("Remote hash matches current evaluation commit, no update needed");
                        return Ok((false, remote_hash));
                    }
                    info!("Remote hash differs from current evaluation commit, update needed");
                } else {
                    info!(eval_id = %last_evaluation, "Last evaluation's commit row missing; treating as update needed");
                }
            } else {
                info!(eval_id = %last_evaluation, "Last evaluation row missing; treating as no previous evaluation");
            }
        } else {
            info!("No previous evaluation found, update needed");
        }

        Ok((true, remote_hash))
    }
}
