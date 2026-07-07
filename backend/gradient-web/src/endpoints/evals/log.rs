/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_org_member;
use crate::authorization::MaybeApiKey;
use crate::error::WebError;
use async_stream::stream;
use axum::Extension;
use axum::extract::{Path, State};
use axum_streams::StreamBodyAs;
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;

use super::EvalAccessContext;

/// The eval's (anchor, derivation-name) pairs, one per build_job. Anchors carry
/// status; the name labels each log line.
async fn eval_anchor_jobs(
    state: &Arc<ServerState>,
    evaluation: EvaluationId,
) -> Result<Vec<(MDerivationBuild, String)>, WebError> {
    let jobs = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .all(&state.web_db)
        .await?;

    let anchor_ids: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();
    let anchors: HashMap<DerivationBuildId, MDerivationBuild> = EDerivationBuild::find()
        .filter(CDerivationBuild::Id.is_in(anchor_ids))
        .all(&state.web_db)
        .await?
        .into_iter()
        .map(|a| (a.id, a))
        .collect();

    let mut out = Vec::with_capacity(jobs.len());
    for job in jobs {
        let Some(anchor) = anchors.get(&job.derivation_build).cloned() else {
            continue;
        };
        let name = match EDerivation::find_by_id(job.derivation)
            .one(&state.web_db)
            .await
        {
            Ok(Some(d)) => d.name,
            _ => String::new(),
        };
        out.push((anchor, name));
    }

    Ok(out)
}

pub async fn post_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> Result<StreamBodyAs<'static>, WebError> {
    let api_key_ref = api_key.as_ref();
    let ctx =
        EvalAccessContext::load(&state, evaluation_id, &Some(user.clone()), api_key_ref).await?;

    // Streaming log access requires org membership (not just public read access).
    if !is_org_member(&state, user.id, ctx.organization_id, api_key_ref).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    let evaluation = ctx.evaluation;

    let stream = stream! {
        let mut last_logs: HashMap<DerivationBuildId, usize> = HashMap::new();

        let past = match eval_anchor_jobs(&state, evaluation.id).await {
            Ok(jobs) => jobs,
            Err(e) => {
                error!(error = %e, "Failed to query past builds");
                return;
            }
        };

        for (anchor, name) in past {
            let log = match gradient_db::latest_attempt_id(&state.web_db, anchor.id).await.unwrap_or(None) {
                Some(key) => state.log_storage.read(key).await.unwrap_or_default(),
                None => String::new(),
            };
            last_logs.insert(anchor.id, log.len());

            yield log
                .split("\n")
                .map(|l| format!("{}> {}", name, l))
                .collect::<Vec<String>>()
                .join("\n");
        }

        loop {
            let current = match eval_anchor_jobs(&state, evaluation.id).await {
                Ok(jobs) => jobs,
                Err(e) => {
                    error!(error = %e, "Failed to query builds");
                    break;
                }
            };

            let building: Vec<(MDerivationBuild, String)> = current
                .iter()
                .filter(|(a, _)| a.status == BuildStatus::Building)
                .cloned()
                .collect();

            if building.is_empty() {
                let any_pending = current
                    .iter()
                    .any(|(a, _)| matches!(a.status, BuildStatus::Building | BuildStatus::Queued));
                // No builds are running or queued. Only end the stream once the
                // evaluation itself has finished: at the very start it is still
                // evaluating and has not created any build_job yet, so breaking
                // here ended the stream before a single line ever streamed.
                if !any_pending {
                    let still_active = EEvaluation::find_by_id(evaluation.id)
                        .one(&state.web_db)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|e| e.status.is_active());
                    if !still_active {
                        yield "".to_string();
                        break;
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }

            for (anchor, name) in building {
                let log = match gradient_db::latest_attempt_id(&state.web_db, anchor.id).await.unwrap_or(None) {
                    Some(key) => state.log_storage.read(key).await.unwrap_or_default(),
                    None => String::new(),
                };
                let last_offset = *last_logs.get(&anchor.id).unwrap_or(&0);
                let log_new = log[last_offset..].to_string();

                if !log_new.is_empty() {
                    last_logs.insert(anchor.id, log.len());
                    yield log_new
                        .split("\n")
                        .map(|l| format!("{}> {}", name, l))
                        .collect::<Vec<String>>()
                        .join("\n");
                } else {
                    last_logs.entry(anchor.id).or_insert(0);
                }
            }
        }
    };

    Ok(StreamBodyAs::json_nl(stream))
}
