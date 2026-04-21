/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::WebResult;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use core::types::input::vec_to_hex;
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::EvalAccessContext;
use super::types::{
    BuildItem, BuildsQuery, EntryPointBrief, EvaluationMessageResponse, EvaluationResponse,
    PaginatedBuilds,
};

pub async fn get_evaluation(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<EvaluationResponse>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user).await?;
    let evaluation = ctx.evaluation;

    let commit_hash = ECommit::find_by_id(evaluation.commit)
        .one(&state.db)
        .await?
        .map(|c| vec_to_hex(&c.hash))
        .unwrap_or_default();

    let all_messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await?;
    let error_count = all_messages
        .iter()
        .filter(|m| m.level == entity::evaluation_message::MessageLevel::Error)
        .count() as u64;
    let warning_count = all_messages
        .iter()
        .filter(|m| m.level == entity::evaluation_message::MessageLevel::Warning)
        .count() as u64;

    // Load entry points with their build statuses.
    let ep_rows = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await?;
    let entry_points = if ep_rows.is_empty() {
        vec![]
    } else {
        let build_ids: Vec<Uuid> = ep_rows.iter().map(|ep| ep.build).collect();
        let builds: std::collections::HashMap<Uuid, entity::build::BuildStatus> = EBuild::find()
            .filter(CBuild::Id.is_in(build_ids))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|b| (b.id, b.status))
            .collect();
        ep_rows
            .into_iter()
            .map(|ep| {
                let build_status = builds
                    .get(&ep.build)
                    .cloned()
                    .unwrap_or(entity::build::BuildStatus::Created);
                EntryPointBrief {
                    id: ep.id,
                    eval: ep.eval,
                    build_status,
                }
            })
            .collect()
    };

    let res = BaseResponse {
        error: false,
        message: EvaluationResponse {
            id: evaluation.id,
            project: evaluation.project,
            project_name: ctx.project_name,
            project_display_name: ctx.project_display_name,
            repository: evaluation.repository,
            commit: commit_hash,
            wildcard: evaluation.wildcard,
            status: evaluation.status,
            previous: evaluation.previous,
            next: evaluation.next,
            created_at: evaluation.created_at,
            error_count,
            warning_count,
            entry_points,
        },
    };

    Ok(Json(res))
}

pub async fn get_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(evaluation_id): Path<Uuid>,
    Query(query): Query<BuildsQuery>,
) -> WebResult<Json<BaseResponse<PaginatedBuilds>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user).await?;
    let evaluation = ctx.evaluation;

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await?;

    let drv_ids: Vec<Uuid> = builds.iter().map(|b| b.derivation).collect();

    let derivations: HashMap<Uuid, MDerivation> = if drv_ids.is_empty() {
        HashMap::new()
    } else {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(drv_ids.clone()))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|d| (d.id, d))
            .collect()
    };

    // has_artefacts is per-derivation: any output of the derivation has build_product rows.
    let has_artefacts_map: HashMap<Uuid, bool> = if drv_ids.is_empty() {
        HashMap::new()
    } else {
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(drv_ids))
            .all(&state.db)
            .await?;
        let output_ids: Vec<Uuid> = outputs.iter().map(|o| o.id).collect();
        let mut m: HashMap<Uuid, bool> = HashMap::new();
        if !output_ids.is_empty() {
            for bp in EBuildProduct::find()
                .filter(CBuildProduct::DerivationOutput.is_in(output_ids))
                .all(&state.db)
                .await?
            {
                if let Some(output) = outputs.iter().find(|o| o.id == bp.derivation_output) {
                    m.insert(output.derivation, true);
                }
            }
        }
        m
    };

    let mut builds: Vec<BuildItem> = builds
        .iter()
        .filter_map(|b| {
            let drv = derivations.get(&b.derivation)?;
            if !drv.derivation_path.ends_with(".drv") {
                return None;
            }
            Some(BuildItem {
                id: b.id,
                name: drv.derivation_path.clone(),
                status: format!("{:?}", b.status),
                has_artefacts: *has_artefacts_map.get(&b.derivation).unwrap_or(&false),
                updated_at: b.updated_at,
                build_time_ms: b.build_time_ms,
            })
        })
        .collect();

    // Sort by status (Building → Queued → Failed → Aborted/DependencyFailed →
    // Completed/Substituted), then by display name. Must match the client-side
    // ordering in `evaluation-log.component.ts::buildStatusOrder`.
    fn status_rank(status: &str) -> u32 {
        match status {
            "Building" => 0,
            "Queued" => 1,
            "Failed" => 2,
            "Aborted" | "DependencyFailed" => 3,
            "Completed" | "Substituted" => 4,
            _ => 99,
        }
    }
    fn display_name(path: &str) -> &str {
        let filename = path.rsplit('/').next().unwrap_or(path);
        let stripped = filename.strip_suffix(".drv").unwrap_or(filename);
        stripped
            .split_once('-')
            .map(|(_, rest)| rest)
            .unwrap_or(stripped)
    }
    builds.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| display_name(&a.name).cmp(display_name(&b.name)))
    });

    let total = builds.len();
    let active_count = builds.iter().filter(|b| status_rank(&b.status) < 4).count();
    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query.limit.unwrap_or(total);
    let page = builds.into_iter().skip(offset).take(limit).collect();

    let res = BaseResponse {
        error: false,
        message: PaginatedBuilds {
            builds: page,
            total,
            active_count,
        },
    };

    Ok(Json(res))
}

/// `GET /evals/{evaluation}/messages`
///
/// Returns all `evaluation_message` rows for an evaluation, each annotated with
/// the list of `entry_point` UUIDs the message is attached to (empty = evaluation-scoped).
pub async fn get_evaluation_messages(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<Vec<EvaluationMessageResponse>>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user).await?;
    let evaluation = ctx.evaluation;

    let messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation.id))
        .order_by(CEvaluationMessage::CreatedAt, Order::Asc)
        .all(&state.db)
        .await?;

    let msg_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();

    // Fetch all entry_point_message join rows for these messages in one query.
    let ep_rows = if msg_ids.is_empty() {
        vec![]
    } else {
        EEntryPointMessage::find()
            .filter(CEntryPointMessage::Message.is_in(msg_ids))
            .all(&state.db)
            .await?
    };

    // Build a map: message_id → [entry_point_id]
    let mut ep_map: std::collections::HashMap<Uuid, Vec<Uuid>> = std::collections::HashMap::new();
    for row in ep_rows {
        ep_map.entry(row.message).or_default().push(row.entry_point);
    }

    let result: Vec<EvaluationMessageResponse> = messages
        .into_iter()
        .map(|m| {
            let entry_points = ep_map.get(&m.id).cloned().unwrap_or_default();
            EvaluationMessageResponse {
                id: m.id,
                level: m.level,
                message: m.message,
                source: m.source,
                created_at: m.created_at,
                entry_points,
            }
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: result,
    }))
}
