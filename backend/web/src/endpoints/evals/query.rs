/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use gradient_core::types::input::vec_to_hex;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::EvalAccessContext;
use super::types::{
    BuildItem, BuildsQuery, EntryPointBrief, EvaluationMessageResponse, EvaluationResponse,
    EvaluationTriggerSummary, PaginatedBuilds,
};
use gradient_core::types::triggers::TriggerType;

pub async fn get_evaluation(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<EvaluationResponse>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let evaluation = ctx.evaluation;

    let commit_hash = ECommit::find_by_id(evaluation.commit)
        .one(&state.web_db)
        .await?
        .map(|c| vec_to_hex(&c.hash))
        .unwrap_or_default();

    let all_messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation.id))
        .all(&state.web_db)
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
        .all(&state.web_db)
        .await?;
    let entry_points = if ep_rows.is_empty() {
        vec![]
    } else {
        let build_ids: Vec<BuildId> = ep_rows.iter().map(|ep| ep.build).collect();
        let builds: std::collections::HashMap<BuildId, entity::build::BuildStatus> = EBuild::find()
            .filter(CBuild::Id.is_in(build_ids))
            .all(&state.web_db)
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
                    .unwrap_or(entity::build::BuildStatus::Queued)
                    .for_api();
                EntryPointBrief {
                    id: ep.id,
                    eval: ep.eval,
                    build_status,
                }
            })
            .collect()
    };

    let waiting_reason = if matches!(
        evaluation.status,
        entity::evaluation::EvaluationStatus::Waiting
    ) {
        evaluation
            .waiting_reason
            .as_ref()
            .and_then(gradient_core::types::WaitingReason::from_json)
    } else {
        None
    };

    // Resolve `evaluation.trigger -> project_trigger.trigger_type` so the eval
    // log page can render the correct "Via" badge. `None` here means the run
    // was started manually (API / UI), not by a project trigger - the frontend
    // renders that as "Manual".
    let trigger = if let Some(trigger_id) = evaluation.trigger {
        EProjectTrigger::find_by_id(trigger_id)
            .one(&state.web_db)
            .await?
            .and_then(|t| {
                TriggerType::try_from(t.trigger_type)
                    .ok()
                    .map(|tt| EvaluationTriggerSummary {
                        id: trigger_id,
                        trigger_type: tt,
                    })
            })
    } else {
        None
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
            trigger,
            waiting_reason,
        },
    };

    Ok(Json(res))
}

/// Maximum number of values per `IN (...)` parameter list. Postgres' wire
/// protocol limits any query to 65 535 bind parameters; 10 000 leaves room
/// for additional filters/joins and avoids overflowing on evaluations with
/// tens of thousands of builds (issue #237).
const IS_IN_CHUNK: usize = 10_000;

fn status_rank(status: entity::build::BuildStatus) -> u32 {
    use entity::build::BuildStatus::*;
    match status {
        Building => 0,
        Created | Queued => 1,
        Failed => 2,
        Aborted | DependencyFailed => 3,
        Completed | Substituted => 4,
    }
}

pub async fn get_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
    Query(query): Query<BuildsQuery>,
) -> WebResult<Json<BaseResponse<PaginatedBuilds>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let evaluation = ctx.evaluation;

    let raw_builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .all(&state.web_db)
        .await?;

    // Followers (`via IS NOT NULL`) are stand-ins for a leader build in another
    // evaluation that's doing the actual work. The follower's own `status`,
    // `updated_at`, `build_time_ms` and even `id` are uninteresting until the
    // leader finishes - surface the leader's row in this list instead so the
    // frontend renders the live build and log endpoints resolve to the right
    // build id. Same-org invariant (see `entity::build::Model::via`) means no
    // cross-org leak.
    let leader_ids: Vec<BuildId> = raw_builds
        .iter()
        .filter_map(|b| b.via)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut leaders: HashMap<BuildId, MBuild> = HashMap::new();
    for chunk in leader_ids.chunks(IS_IN_CHUNK) {
        let rows = EBuild::find()
            .filter(CBuild::Id.is_in(chunk.to_vec()))
            .all(&state.web_db)
            .await?;
        for row in rows {
            leaders.insert(row.id, row);
        }
    }
    let builds: Vec<MBuild> = raw_builds
        .into_iter()
        .map(|b| match b.via.and_then(|id| leaders.get(&id)) {
            Some(leader) => leader.clone(),
            None => b,
        })
        .collect();

    // Distinct derivations referenced by builds. Deduping cuts the IN-list down
    // by the leader/follower factor (often 2–10x in large evals).
    let drv_ids: Vec<DerivationId> = builds
        .iter()
        .map(|b| b.derivation)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut derivations: HashMap<DerivationId, MDerivation> = HashMap::new();
    for chunk in drv_ids.chunks(IS_IN_CHUNK) {
        let rows = EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk.to_vec()))
            .all(&state.web_db)
            .await?;
        for row in rows {
            derivations.insert(row.id, row);
        }
    }

    // Sort by status (Building → Queued → Failed → Aborted/DependencyFailed →
    // Completed/Substituted), then by derivation name. Mirrors the client-side
    // ordering in `evaluation-log.component.ts::buildStatusOrder`.
    let mut sorted: Vec<(u32, &str, &MBuild)> = builds
        .iter()
        .filter_map(|b| {
            let drv = derivations.get(&b.derivation)?;
            Some((status_rank(b.status.for_api()), drv.name.as_str(), b))
        })
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));

    let total = sorted.len();
    let active_count = sorted.iter().filter(|(rank, _, _)| *rank < 4).count();

    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query.limit.unwrap_or(total.saturating_sub(offset));
    let page_slice: Vec<&(u32, &str, &MBuild)> = sorted.iter().skip(offset).take(limit).collect();

    // Hydrate `has_artefacts` only for the page. Bounded by `limit`, so the
    // `IN` clause is safe regardless of evaluation size.
    let page_drv_ids: Vec<DerivationId> = page_slice
        .iter()
        .map(|(_, _, b)| b.derivation)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut has_artefacts: HashSet<DerivationId> = HashSet::new();
    if !page_drv_ids.is_empty() {
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(page_drv_ids.clone()))
            .all(&state.web_db)
            .await?;
        let output_to_drv: HashMap<DerivationOutputId, DerivationId> =
            outputs.iter().map(|o| (o.id, o.derivation)).collect();
        let output_ids: Vec<DerivationOutputId> = outputs.iter().map(|o| o.id).collect();
        for chunk in output_ids.chunks(IS_IN_CHUNK) {
            let products = EBuildProduct::find()
                .filter(CBuildProduct::DerivationOutput.is_in(chunk.to_vec()))
                .all(&state.web_db)
                .await?;
            for bp in products {
                if let Some(&drv) = output_to_drv.get(&bp.derivation_output) {
                    has_artefacts.insert(drv);
                }
            }
        }
    }

    let page: Vec<BuildItem> = page_slice
        .into_iter()
        .map(|(_, _, b)| {
            let drv = derivations
                .get(&b.derivation)
                .expect("derivation hydrated above");
            BuildItem {
                id: b.id,
                name: drv.store_path(),
                status: format!("{:?}", b.status.for_api()),
                has_artefacts: has_artefacts.contains(&b.derivation),
                updated_at: b.updated_at,
                build_time_ms: b.build_time_ms,
            }
        })
        .collect();

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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<Vec<EvaluationMessageResponse>>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let evaluation = ctx.evaluation;

    let messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation.id))
        .order_by(CEvaluationMessage::CreatedAt, Order::Asc)
        .all(&state.web_db)
        .await?;

    let msg_ids: Vec<EvaluationMessageId> = messages.iter().map(|m| m.id).collect();

    // Fetch all entry_point_message join rows for these messages in one query.
    let ep_rows = if msg_ids.is_empty() {
        vec![]
    } else {
        EEntryPointMessage::find()
            .filter(CEntryPointMessage::Message.is_in(msg_ids))
            .all(&state.web_db)
            .await?
    };

    // Build a map: message_id → [entry_point_id]
    let mut ep_map: std::collections::HashMap<EvaluationMessageId, Vec<EntryPointId>> =
        std::collections::HashMap::new();
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

    Ok(ok_json(result))
}
