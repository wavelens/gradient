/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::user_is_org_member;
use crate::error::WebError;
use async_stream::stream;
use axum::Extension;
use axum::extract::{Path, State};
use axum_streams::StreamBodyAs;
use core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

use super::EvalAccessContext;
use super::types::drv_display_name;

pub async fn post_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> Result<StreamBodyAs<'static>, WebError> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &Some(user.clone())).await?;

    // Streaming log access requires org membership (not just public read access).
    if !user_is_org_member(&state, user.id, ctx.organization_id).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    let evaluation = ctx.evaluation;

    let condition = Condition::all()
        .add(CBuild::Evaluation.eq(evaluation.id))
        .add(CBuild::Status.eq(entity::build::BuildStatus::Building));

    let stream = stream! {
        let mut last_logs: HashMap<Uuid, usize> = HashMap::new();

        let past_builds = match EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await
        {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, "Failed to query past builds");
                return;
            }
        };

        for build in past_builds {
            let drv_path = match EDerivation::find_by_id(build.derivation).one(&state.db).await {
                Ok(Some(d)) => d.derivation_path,
                _ => String::new(),
            };
            let name = drv_display_name(&drv_path);
            let log = state.log_storage.read(build.log_id.unwrap_or(build.id)).await.unwrap_or_default();
            last_logs.insert(build.id, log.len());

            // TODO: Chunkify past log
            yield log
                .split("\n")
                .map(|l| format!("{}> {}", name, l))
                .collect::<Vec<String>>()
                .join("\n");
        }

        loop {
            let builds = match EBuild::find()
                .filter(condition.clone())
                .all(&state.db)
                .await {
                Ok(b) => b,
                Err(e) => {
                    error!(error = %e, "Failed to query builds");
                    break;
                }
            };

            if builds.is_empty() {
                let all_builds = match EBuild::find()
                    .filter(
                        Condition::all()
                            .add(CBuild::Evaluation.eq(evaluation.id))
                            .add(
                                Condition::any()
                                    .add(CBuild::Status.eq(entity::build::BuildStatus::Building))
                                    .add(CBuild::Status.eq(entity::build::BuildStatus::Queued)),
                            ),
                    )
                    .one(&state.db)
                    .await {
                    Ok(b) => b,
                    Err(e) => {
                        error!(error = %e, "Failed to query all builds");
                        break;
                    }
                };

                if all_builds.is_none() {
                    yield "".to_string();
                    break;
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }

            for build in builds {
                let drv_path = match EDerivation::find_by_id(build.derivation).one(&state.db).await {
                    Ok(Some(d)) => d.derivation_path,
                    _ => String::new(),
                };
                let name = drv_display_name(&drv_path);
                let log = state.log_storage.read(build.log_id.unwrap_or(build.id)).await.unwrap_or_default();
                let last_offset = *last_logs.get(&build.id).unwrap_or(&0);
                let log_new = log[last_offset..].to_string();

                if !log_new.is_empty() {
                    last_logs.insert(build.id, log.len());
                    yield log_new
                        .split("\n")
                        .map(|l| format!("{}> {}", name, l))
                        .collect::<Vec<String>>()
                        .join("\n");
                } else {
                    last_logs.entry(build.id).or_insert(0);
                }
            }
        }
    };

    Ok(StreamBodyAs::json_nl(stream))
}
