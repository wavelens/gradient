/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use core::types::input::vec_to_hex;
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::types::{BuildItem, BuildsQuery, EvaluationMessageResponse, EvaluationResponse, PaginatedBuilds};

pub async fn get_evaluation(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<EvaluationResponse>>> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let (organization_id, project_name) = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation_id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        let name = project.name.clone();
        (project.organization, Some(name))
    } else {
        let org_id = EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization;
        (org_id, None)
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Evaluation"));
    }

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

    let res = BaseResponse {
        error: false,
        message: EvaluationResponse {
            id: evaluation.id,
            project: evaluation.project,
            project_name,
            repository: evaluation.repository,
            commit: commit_hash,
            wildcard: evaluation.wildcard,
            status: evaluation.status,
            previous: evaluation.previous,
            next: evaluation.next,
            created_at: evaluation.created_at,
            error_count,
            warning_count,
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
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation_id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Evaluation"));
    }

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

    // has_artefacts is per-derivation: any output of the derivation has artefacts.
    let has_artefacts_map: HashMap<Uuid, bool> = if drv_ids.is_empty() {
        HashMap::new()
    } else {
        let mut m: HashMap<Uuid, bool> = HashMap::new();
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(drv_ids))
            .filter(CDerivationOutput::HasArtefacts.eq(true))
            .all(&state.db)
            .await?;
        for o in outputs {
            m.insert(o.derivation, true);
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
        stripped.split_once('-').map(|(_, rest)| rest).unwrap_or(stripped)
    }
    builds.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| display_name(&a.name).cmp(display_name(&b.name)))
    });

    let total = builds.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query.limit.unwrap_or(total);
    let page = builds.into_iter().skip(offset).take(limit).collect();

    let res = BaseResponse {
        error: false,
        message: PaginatedBuilds {
            builds: page,
            total,
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
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| WebError::InternalServerError("Evaluation data inconsistency".to_string()))?
            .organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| WebError::InternalServerError("Direct build data inconsistency".to_string()))?
            .organization
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::InternalServerError("Organization data inconsistency".to_string()))?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Evaluation"));
    }

    let messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
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
