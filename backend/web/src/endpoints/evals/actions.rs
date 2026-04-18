/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::types::*;
use std::sync::Arc;
use uuid::Uuid;

use super::EvalAccessContext;
use super::types::MakeEvaluationRequest;

pub async fn post_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<scheduler::Scheduler>>,
    Path(evaluation_id): Path<Uuid>,
    Json(body): Json<MakeEvaluationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &Some(user.clone())).await?;

    // Mutations require explicit org membership even when the org is public.
    if !user_is_org_member(&state, user.id, ctx.organization_id).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    if body.method == "abort" {
        scheduler.abort_evaluation(ctx.evaluation).await;
    }

    Ok(Json(BaseResponse {
        error: false,
        message: "Success".to_string(),
    }))
}
