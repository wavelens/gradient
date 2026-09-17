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

use crate::actions::{dispatch_build_event, dispatch_evaluation_event};
use crate::context::CiContext;
use crate::reactions::react_to_source_comment_on_terminal;
use crate::reporting::{eval_kind_str, evaluation_event_for_status};
use gradient_db::{DbContext, StatusReactor};
use gradient_forge::ForgeRegistry;
use gradient_notify::EmailSender;
use gradient_types::*;
use std::sync::Arc;

#[derive(Debug)]
pub struct CiStatusReactor {
    http: reqwest::Client,
    forge: ForgeRegistry,
    email: Arc<dyn EmailSender>,
}

impl CiStatusReactor {
    pub fn new(http: reqwest::Client, email: Arc<dyn EmailSender>) -> Self {
        Self {
            http,
            forge: ForgeRegistry::with_builtin(),
            email,
        }
    }

    fn ci_context(&self, db: &DbContext) -> CiContext {
        CiContext {
            db: db.clone(),
            http: self.http.clone(),
            forge: self.forge.clone(),
            email: self.email.clone(),
        }
    }
}

#[async_trait]
impl StatusReactor for CiStatusReactor {
    async fn on_build_status_changed(
        &self,
        db: &DbContext,
        build_job: MBuildJob,
        status: BuildStatus,
    ) {
        let Some(event) = crate::reporting::build_event_for_status(status) else {
            return;
        };

        let ctx = self.ci_context(db);

        let evaluation = match EEvaluation::find_by_id(build_job.evaluation)
            .one(&ctx.db.worker_db)
            .await
        {
            Ok(Some(e)) => e,
            Ok(None) => {
                warn!(evaluation_id = %build_job.evaluation, "Evaluation not found for action dispatch");
                return;
            }
            Err(e) => {
                error!(error = %e, evaluation_id = %build_job.evaluation, "DB error looking up evaluation for action dispatch");
                return;
            }
        };

        let task_id = match evaluation.task {
            Some(id) => id,
            None => return,
        };

        let derivation_path = EDerivation::find_by_id(build_job.derivation)
            .one(&ctx.db.worker_db)
            .await
            .ok()
            .flatten()
            .map(|d| d.store_path());

        let payload = serde_json::json!({
            "build_id": build_job.id,
            "evaluation_id": build_job.evaluation,
            "derivation_path": derivation_path,
            "status": event,
            "evaluation_kind": eval_kind_str(evaluation.kind),
        });

        dispatch_build_event(&ctx, task_id, event, payload).await;
    }

    async fn on_eval_terminal(
        &self,
        db: &DbContext,
        evaluation: MEvaluation,
        status: EvaluationStatus,
    ) {
        let event = evaluation_event_for_status(status);

        let task_id = match evaluation.task {
            Some(id) => id,
            None => return,
        };

        let ctx = self.ci_context(db);

        let payload = serde_json::json!({
            "evaluation_id": evaluation.id,
            "task_id": evaluation.task,
            "repository": evaluation.repository,
            "status": event,
            "evaluation_kind": eval_kind_str(evaluation.kind),
        });

        dispatch_evaluation_event(&ctx, task_id, event, payload).await;

        react_to_source_comment_on_terminal(&ctx, task_id, &evaluation, status).await;
    }
}
