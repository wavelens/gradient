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
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
};

/// When the condition holds, create one `input_update` evaluation per the
/// `OpenPr` action's granularity. Best-effort: returns the created evaluation
/// ids, or an empty vec when the condition is not met.
pub async fn maybe_trigger_input_update<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    base_commit_hash: Vec<u8>,
    trigger: Option<ProjectTriggerId>,
) -> Result<Vec<EvaluationId>, TriggerError> {
    let Some(action) = EProjectAction::find()
        .filter(CProjectAction::Project.eq(project.id))
        .filter(CProjectAction::Active.eq(true))
        .filter(CProjectAction::ActionType.eq(ActionType::OpenPr))
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

    let active_codes: Vec<i32> = EvaluationStatus::ACTIVE
        .iter()
        .copied()
        .map(i32::from)
        .collect();
    let already_running = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Kind.eq(EvaluationKind::InputUpdate))
        .filter(CEvaluation::Status.is_in(active_codes))
        .one(db)
        .await?;
    if already_running.is_some() {
        return Ok(Vec::new());
    }

    let base_commit: String = base_commit_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    // Globs can only be expanded worker-side against flake.lock. Under PerInput
    // they are collected into a single discovery eval whose matches fan out into
    // one per-input eval each; under PerRun they ride along in the one eval.
    let (globs, literals): (Vec<String>, Vec<String>) = tracked
        .into_iter()
        .partition(|n| gradient_util::glob::is_pattern(n));

    let mut created = Vec::new();
    match granularity {
        PrGranularity::PerRun => {
            let mut all = literals;
            all.extend(globs);
            if !all.is_empty() {
                created.push(
                    create_input_update_eval(db, project, base_commit, all, false, trigger).await?,
                );
            }
        }
        PrGranularity::PerInput => {
            for lit in literals {
                created.push(
                    create_input_update_eval(
                        db,
                        project,
                        base_commit.clone(),
                        vec![lit],
                        false,
                        trigger,
                    )
                    .await?,
                );
            }
            if !globs.is_empty() {
                created.push(
                    create_input_update_eval(db, project, base_commit, globs, true, trigger)
                        .await?,
                );
            }
        }
    }

    Ok(created)
}

/// Create one `input_update` evaluation for `target` with a blank commit (the
/// PR commit does not exist until the branch is force-pushed) plus its sidecar.
/// `discover_only` marks a glob-discovery run that opens no PR.
pub async fn create_input_update_eval<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    base_commit: String,
    target: Vec<String>,
    discover_only: bool,
    trigger: Option<ProjectTriggerId>,
) -> Result<EvaluationId, TriggerError> {
    let now = gradient_types::now();
    let commit = MCommit {
        id: CommitId::now_v7(),
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
        base_commit,
        generator: "flake_lock".to_owned(),
        target_inputs: serde_json::json!(target),
        candidate_lock: None,
        bumped_inputs: None,
        discover_only,
        created_at: now,
        updated_at: now,
    }
    .into_active_model()
    .insert(db)
    .await?;

    Ok(evaluation.id)
}

/// Fan a discovery eval's matched inputs out into one per-input update eval each,
/// skipping inputs that already have an active update eval for the project.
pub async fn fan_out_expansion<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    base_commit: String,
    matched: Vec<String>,
    trigger: Option<ProjectTriggerId>,
) -> Result<Vec<EvaluationId>, TriggerError> {
    let active_codes: Vec<i32> = EvaluationStatus::ACTIVE
        .iter()
        .copied()
        .map(i32::from)
        .collect();
    let active = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Kind.eq(EvaluationKind::InputUpdate))
        .filter(CEvaluation::Status.is_in(active_codes))
        .all(db)
        .await?;
    let active_ids: Vec<EvaluationId> = active.iter().map(|e| e.id).collect();
    let active_targets: std::collections::BTreeSet<String> = EEvaluationInputUpdate::find()
        .filter(CEvaluationInputUpdate::Evaluation.is_in(active_ids))
        .all(db)
        .await?
        .into_iter()
        .filter(|s| !s.discover_only)
        .filter_map(|s| s.target_inputs.as_array().cloned())
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let mut created = Vec::new();
    for input in matched {
        if active_targets.contains(&input) {
            continue;
        }
        created.push(
            create_input_update_eval(
                db,
                project,
                base_commit.clone(),
                vec![input],
                false,
                trigger,
            )
            .await?,
        );
    }

    Ok(created)
}
