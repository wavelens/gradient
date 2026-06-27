/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Creates `input_update` evaluations when a trigger fires on a project that
//! has an `OpenPr` action and tracked flake inputs. The cheap server-side
//! condition gates creation; the worker decides whether there is actually
//! anything to bump and short-circuits empty runs.

use super::TriggerError;
use gradient_types::*;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter};

/// When the condition holds, create one `input_update` evaluation per the
/// `OpenPr` action's granularity. Best-effort: returns the created evaluation
/// ids, or an empty vec when the condition is not met.
pub async fn maybe_trigger_input_update<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    trigger: Option<ProjectTriggerId>,
) -> Result<Vec<EvaluationId>, TriggerError> {
    let Some(action) = EProjectAction::find()
        .filter(CProjectAction::Project.eq(project.id))
        .filter(CProjectAction::Active.eq(true))
        .filter(CProjectAction::ActionType.eq(ActionType::OpenPr.to_i16()))
        .one(db)
        .await?
    else {
        return Ok(Vec::new());
    };

    let Ok(ActionConfig::OpenPr { granularity, .. }) =
        serde_json::from_value::<ActionConfig>(action.config.clone())
    else {
        return Ok(Vec::new());
    };

    let overrides = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Project.eq(project.id))
        .all(db)
        .await?;
    // Safety gate: a url-pinned override anywhere on the project blocks the run.
    if overrides.iter().any(|o| o.url.is_some()) {
        return Ok(Vec::new());
    }

    let tracked: Vec<String> = overrides.into_iter().map(|o| o.input_name).collect();
    if tracked.is_empty() {
        return Ok(Vec::new());
    }

    let active_codes: Vec<i32> = EvaluationStatus::ACTIVE.iter().copied().map(i32::from).collect();
    let already_running = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Kind.eq(EvaluationKind::InputUpdate))
        .filter(CEvaluation::Status.is_in(active_codes))
        .one(db)
        .await?;
    if already_running.is_some() {
        return Ok(Vec::new());
    }

    let target_sets: Vec<Vec<String>> = match granularity {
        PrGranularity::PerRun => vec![tracked],
        PrGranularity::PerInput => tracked.into_iter().map(|n| vec![n]).collect(),
    };

    let base_commit: String = commit_hash.iter().map(|b| format!("{b:02x}")).collect();
    let now = gradient_types::now();
    let mut created = Vec::with_capacity(target_sets.len());

    for target in target_sets {
        let commit = MCommit {
            id: CommitId::now_v7(),
            message: commit_message.clone().unwrap_or_default(),
            hash: commit_hash.clone(),
            author_name: author_name.clone().unwrap_or_default(),
            ..Default::default()
        }
        .into_active_model()
        .insert(db)
        .await?;

        let evaluation = MEvaluation {
            id: EvaluationId::now_v7(),
            project: Some(project.id),
            repository: project.repository.clone(),
            commit: commit.id,
            wildcard: project.wildcard.clone(),
            status: EvaluationStatus::Queued,
            kind: EvaluationKind::InputUpdate,
            concurrent: true,
            trigger,
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
        .into_active_model()
        .insert(db)
        .await?;

        MEvaluationInputUpdate {
            id: EvaluationInputUpdateId::now_v7(),
            evaluation: evaluation.id,
            base_commit: base_commit.clone(),
            generator: "flake_lock".to_owned(),
            target_inputs: serde_json::json!(target),
            candidate_lock: None,
            bumped_inputs: None,
            created_at: now,
            updated_at: now,
        }
        .into_active_model()
        .insert(db)
        .await?;

        created.push(evaluation.id);
    }

    Ok(created)
}
