/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `ci` side of the [`StatusReactor`] inversion: turns terminal build/evaluation
//! statuses into forge events and `/gradient` PR-comment reactions. `db` hands
//! us a [`DbContext`]; we pair it with our own HTTP client to form the
//! [`CiContext`] the dispatch helpers need.

use async_trait::async_trait;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::EntityTrait;
use tracing::{error, warn};

use crate::ci::actions::{dispatch_build_event, dispatch_evaluation_event, reporter_for_project};
use crate::ci::context::CiContext;
use crate::ci::{ReactionKind, ReactionTarget};
use crate::db::{DbContext, StatusReactor};
use crate::types::*;

#[derive(Debug)]
pub struct CiStatusReactor {
    http: reqwest::Client,
}

impl CiStatusReactor {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    fn ci_context(&self, db: &DbContext) -> CiContext {
        CiContext {
            db: db.clone(),
            http: self.http.clone(),
        }
    }
}

#[async_trait]
impl StatusReactor for CiStatusReactor {
    async fn on_build_terminal(&self, db: &DbContext, build: MBuild, status: BuildStatus) {
        let event = match status {
            BuildStatus::Queued => "build.queued",
            BuildStatus::Building => "build.started",
            BuildStatus::Completed => "build.completed",
            BuildStatus::FailedPermanent => "build.failed",
            BuildStatus::FailedTimeout => "build.failed",
            BuildStatus::FailedTransient => "build.failed_transient",
            BuildStatus::Substituted => "build.substituted",
            BuildStatus::Created | BuildStatus::Aborted | BuildStatus::DependencyFailed => return,
        };

        let ctx = self.ci_context(db);

        let evaluation = match EEvaluation::find_by_id(build.evaluation)
            .one(&ctx.db.worker_db)
            .await
        {
            Ok(Some(e)) => e,
            Ok(None) => {
                warn!(evaluation_id = %build.evaluation, "Evaluation not found for action dispatch");
                return;
            }
            Err(e) => {
                error!(error = %e, evaluation_id = %build.evaluation, "DB error looking up evaluation for action dispatch");
                return;
            }
        };

        let project_id = match evaluation.project {
            Some(id) => id,
            None => return,
        };

        let derivation_path = EDerivation::find_by_id(build.derivation)
            .one(&ctx.db.worker_db)
            .await
            .ok()
            .flatten()
            .map(|d| d.store_path());

        let payload = serde_json::json!({
            "build_id": build.id,
            "evaluation_id": build.evaluation,
            "derivation_path": derivation_path,
            "status": event,
        });

        dispatch_build_event(&ctx, project_id, event, payload).await;
    }

    async fn on_eval_terminal(
        &self,
        db: &DbContext,
        evaluation: MEvaluation,
        status: EvaluationStatus,
    ) {
        let event = match status {
            EvaluationStatus::Queued => "evaluation.queued",
            EvaluationStatus::Fetching
            | EvaluationStatus::EvaluatingFlake
            | EvaluationStatus::EvaluatingDerivation => "evaluation.started",
            EvaluationStatus::Building => "evaluation.building",
            EvaluationStatus::Waiting => "evaluation.waiting",
            EvaluationStatus::Completed => "evaluation.completed",
            EvaluationStatus::Failed => "evaluation.failed",
            EvaluationStatus::Aborted => "evaluation.aborted",
        };

        let project_id = match evaluation.project {
            Some(id) => id,
            None => return,
        };

        let ctx = self.ci_context(db);

        let payload = serde_json::json!({
            "evaluation_id": evaluation.id,
            "project_id": evaluation.project,
            "repository": evaluation.repository,
            "status": event,
        });

        dispatch_evaluation_event(&ctx, project_id, event, payload).await;

        react_to_source_comment_on_terminal(&ctx, project_id, &evaluation, status).await;
    }
}

/// Post a thumbs-up/-down reaction on the `/gradient` PR comment that triggered
/// this evaluation, once it reaches a terminal status. Best-effort.
async fn react_to_source_comment_on_terminal(
    ctx: &CiContext,
    project_id: ProjectId,
    evaluation: &MEvaluation,
    status: EvaluationStatus,
) {
    let kind = match status {
        EvaluationStatus::Completed => ReactionKind::ThumbsUp,
        EvaluationStatus::Failed | EvaluationStatus::Aborted => ReactionKind::ThumbsDown,
        _ => return,
    };
    let Some(raw) = evaluation.source_comment.as_ref() else {
        return;
    };
    let Some(target) = parse_source_comment(raw) else {
        warn!(
            evaluation_id = %evaluation.id,
            "evaluation.source_comment present but malformed; skipping reaction"
        );
        return;
    };
    let reporter = match reporter_for_project(ctx, project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %project_id, "resolving reporter for terminal-status reaction");
            return;
        }
    };
    if let Err(e) = reporter.add_reaction(&target, kind).await {
        warn!(error = %e, %project_id, ?kind, "/gradient terminal reaction post failed");
    }
}

fn parse_source_comment(value: &serde_json::Value) -> Option<ReactionTarget> {
    let owner = value.get("owner")?.as_str()?.to_string();
    let repo = value.get("repo")?.as_str()?.to_string();
    let pr_number = value.get("pr_number")?.as_u64()?;
    let comment_id = value.get("comment_id")?.as_i64()?;
    Some(ReactionTarget {
        owner,
        repo,
        pr_number,
        comment_id,
    })
}
