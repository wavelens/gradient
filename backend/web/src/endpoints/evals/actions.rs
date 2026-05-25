/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::is_org_member;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::types::*;
use std::sync::Arc;

use super::EvalAccessContext;
use super::types::MakeEvaluationRequest;

pub async fn post_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<scheduler::Scheduler>>,
    Path(evaluation_id): Path<EvaluationId>,
    Json(body): Json<MakeEvaluationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let api_key_ref = api_key.as_ref();
    let ctx =
        EvalAccessContext::load(&state, evaluation_id, &Some(user.clone()), api_key_ref).await?;

    // Mutations require explicit org membership even when the org is public.
    if !is_org_member(&state, user.id, ctx.organization_id, api_key_ref).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    if body.method == "abort" {
        scheduler.abort_evaluation(ctx.evaluation).await;
    }

    Ok(ok_json("Success".to_string()))
}
