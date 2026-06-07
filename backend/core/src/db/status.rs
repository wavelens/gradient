/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared evaluation and build status helpers.
//!
//! Extracted here so both the `evaluator` and `builder` crates can call them
//! without introducing a dependency between the two.

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, IntoActiveModel,
    QueryFilter,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::ci::actions::{dispatch_build_event, dispatch_evaluation_event};
use crate::state_machine::{BuildStateMachine, EvalStateMachine};
use crate::types::*;

/// Compress a finalized build log into zstd chunks, persist the chunk index,
/// and drop the inline copy. Best-effort: failures are logged, never propagated.
pub async fn finalize_build_log(state: &Arc<ServerState>, log_id: entity::ids::BuildId) {
    let log_text = state.log_storage.read(log_id).await.unwrap_or_default();
    if log_text.is_empty() {
        return;
    }
    let descs = match crate::storage::log_chunk::compress_and_store_chunks(
        state.log_storage.as_ref(),
        log_id,
        &log_text,
        state.config.storage.log_chunk_bytes,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, build_id = %log_id, "Failed to chunk build log");
            return;
        }
    };
    if let Err(e) = replace_log_chunk_index(&state.worker_db, log_id, &descs).await {
        error!(error = %e, build_id = %log_id, "Failed to write log chunk index");
        return;
    }
    if let Err(e) = state.log_storage.delete_inline_log(log_id).await {
        warn!(error = %e, build_id = %log_id, "Failed to drop inline log after chunking");
    }
}

/// Replace the `build_log_chunk` rows for `log_id` with `descs` (idempotent).
async fn replace_log_chunk_index(
    db: &impl ConnectionTrait,
    log_id: entity::ids::BuildId,
    descs: &[crate::storage::log_chunk::StoredChunkDesc],
) -> Result<(), sea_orm::DbErr> {
    use entity::build_log_chunk::{ActiveModel, Column, Entity};
    Entity::delete_many()
        .filter(Column::Build.eq(log_id))
        .exec(db)
        .await?;
    if descs.is_empty() {
        return Ok(());
    }
    let rows: Vec<ActiveModel> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| ActiveModel {
            id: Set(entity::ids::BuildLogChunkId::now_v7()),
            build: Set(log_id),
            chunk_index: Set(i as i32),
            byte_start: Set(d.byte_start as i64),
            byte_len: Set(d.byte_len as i32),
            line_start: Set(d.line_start as i64),
            line_count: Set(d.line_count as i32),
            compressed_size: Set(d.compressed_size as i32),
            color_prefix: Set(d.color_prefix.clone()),
        })
        .collect();
    Entity::insert_many(rows).exec(db).await?;
    Ok(())
}

pub const PHASE_SUBJECT_BUILD: i16 = 0;
pub const PHASE_SUBJECT_EVALUATION: i16 = 1;

/// Append-only record of a build/evaluation phase transition. Best-effort:
/// failures are logged, never propagated, so instrumentation can't break a
/// status transition.
pub async fn record_phase_event(
    db: &impl ConnectionTrait,
    subject_kind: i16,
    subject_id: uuid::Uuid,
    phase: i16,
    worker_id: Option<String>,
    at: chrono::NaiveDateTime,
) {
    let ev = entity::phase_event::ActiveModel {
        id: Set(entity::ids::PhaseEventId::now_v7()),
        subject_kind: Set(subject_kind),
        subject_id: Set(subject_id),
        phase: Set(phase),
        event: Set(0),
        at: Set(at),
        worker_id: Set(worker_id),
        detail: Set(None),
    };
    if let Err(e) = entity::phase_event::Entity::insert(ev).exec(db).await {
        warn!(error = %e, "failed to record phase_event");
    }
}

pub async fn update_build_status(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    if build.status == status {
        return build;
    }

    match BuildStateMachine::validate(build.status, status) {
        Ok(_) => {}
        Err(e) => {
            // Loud: a rejected transition usually means the build is stuck
            // in a state the next event can't legally move it from - e.g.
            // a JobFailed arriving while the build is still `Queued`
            // because the worker's `Building` JobUpdate was lost / never
            // sent. Without this we'd silently drop the failure and the UI
            // would show the build hanging in `Queued` / `Building` forever.
            error!(
                build_id = %build.id,
                from = ?build.status,
                to = ?status,
                error = %e,
                "Skipping invalid build status transition - investigate: status update lost or out of order"
            );
            return build;
        }
    }

    info!(build_id = %build.id, from = ?build.status, to = ?status, "build status transition");

    let mut active_build: ABuild = build.clone().into_active_model();

    let event_status = status;
    let now = crate::types::now();
    // When transitioning out of `Building` into a terminal state, record the
    // elapsed wall-clock time. `build.updated_at` is the timestamp of the
    // previous transition (into `Building` by `Scheduler::handle_build_status_update`).
    if build.status == BuildStatus::Building
        && matches!(
            status,
            BuildStatus::Completed
                | BuildStatus::FailedPermanent
                | BuildStatus::FailedTimeout
                | BuildStatus::Aborted
                | BuildStatus::DependencyFailed
        )
        && build.build_time_ms.is_none()
    {
        let elapsed_ms = (now - build.updated_at).num_milliseconds().max(0);
        active_build.build_time_ms = Set(Some(elapsed_ms));
    }
    active_build.status = Set(status);
    active_build.updated_at = Set(now);
    if status == BuildStatus::Queued && build.queued_at.is_none() {
        active_build.queued_at = Set(Some(now));
    }
    if status == BuildStatus::Building {
        active_build.build_started_at = Set(Some(now));
    }
    if matches!(
        status,
        BuildStatus::Completed
            | BuildStatus::Substituted
            | BuildStatus::FailedPermanent
            | BuildStatus::FailedTimeout
            | BuildStatus::Aborted
            | BuildStatus::DependencyFailed
    ) {
        active_build.build_finished_at = Set(Some(now));
    }

    match active_build.update(&state.worker_db).await {
        Ok(updated_build) => {
            let action_state = Arc::clone(&state);
            let action_build = updated_build.clone();
            state.shutdown.spawn(async move {
                dispatch_build_event_for_status(&action_state, action_build, event_status).await;
            });

            let pe_state = Arc::clone(&state);
            let pe_worker = updated_build.worker.clone();
            let pe_id = updated_build.id.into_inner();
            state.shutdown.spawn(async move {
                record_phase_event(
                    &pe_state.worker_db,
                    PHASE_SUBJECT_BUILD,
                    pe_id,
                    i32::from(event_status) as i16,
                    pe_worker,
                    now,
                )
                .await;
            });

            // On terminal state, compress the build log into zstd chunks and
            // record the chunk index, then drop the inline copy so the chunks
            // are the sole at-rest representation.
            if matches!(
                updated_build.status,
                BuildStatus::Completed
                    | BuildStatus::Substituted
                    | BuildStatus::FailedPermanent
                    | BuildStatus::FailedTimeout
                    | BuildStatus::Aborted
                    | BuildStatus::DependencyFailed
            ) {
                let log_state = Arc::clone(&state);
                let log_id = updated_build.log_id.unwrap_or(updated_build.id);
                state.shutdown.spawn(async move {
                    finalize_build_log(&log_state, log_id).await;
                });
            }

            updated_build
        }
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to update build status");
            build
        }
    }
}

pub async fn update_evaluation_status(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) -> MEvaluation {
    // The state machine validates the transition locally. The filtered update_many
    // below also guards atomically in the DB, so concurrent aborts cannot be
    // clobbered by an in-flight evaluator.
    match EvalStateMachine::validate(evaluation.status, status) {
        Ok(_) => {}
        Err(e) => {
            warn!(evaluation_id = %evaluation.id, error = %e, "Skipping invalid evaluation status transition");
            return evaluation;
        }
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, "Updating evaluation status");

    let event_status = status;
    let now = crate::types::now();

    let mut update = EEvaluation::update_many()
        .col_expr(CEvaluation::Status, sea_orm::sea_query::Expr::value(status))
        .col_expr(CEvaluation::UpdatedAt, sea_orm::sea_query::Expr::value(now));

    if !matches!(status, EvaluationStatus::Waiting) {
        update = update.col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(Option::<serde_json::Value>::None),
        );
    }

    let phase_col = match status {
        EvaluationStatus::Fetching => Some(CEvaluation::FetchStartedAt),
        EvaluationStatus::EvaluatingFlake => Some(CEvaluation::EvalFlakeStartedAt),
        EvaluationStatus::EvaluatingDerivation => Some(CEvaluation::EvalDrvStartedAt),
        EvaluationStatus::Building => Some(CEvaluation::BuildingStartedAt),
        EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted => {
            Some(CEvaluation::FinishedAt)
        }
        _ => None,
    };
    if let Some(col) = phase_col {
        update = update.col_expr(col, sea_orm::sea_query::Expr::value(now));
    }

    let update_result = update
        .filter(CEvaluation::Id.eq(evaluation.id))
        .filter(
            Condition::all()
                .add(CEvaluation::Status.ne(EvaluationStatus::Aborted))
                .add(CEvaluation::Status.ne(EvaluationStatus::Failed))
                .add(CEvaluation::Status.ne(EvaluationStatus::Completed)),
        )
        .exec(&state.worker_db)
        .await;

    match update_result {
        Ok(res) if res.rows_affected == 0 => {
            // Row was concurrently transitioned to a terminal state -
            // honor it and return the fresh value instead of clobbering.
            return EEvaluation::find_by_id(evaluation.id)
                .one(&state.worker_db)
                .await
                .ok()
                .flatten()
                .unwrap_or(evaluation);
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to update evaluation status");
            return evaluation;
        }
        Ok(_) => {}
    }

    let updated_eval = EEvaluation::find_by_id(evaluation.id)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let mut e = evaluation.clone();
            e.status = status;
            e.updated_at = now;
            e
        });

    let action_state = Arc::clone(&state);
    let action_eval = updated_eval.clone();
    state.shutdown.spawn(async move {
        dispatch_evaluation_event_for_status(&action_state, action_eval, event_status).await;
    });

    let pe_state = Arc::clone(&state);
    let pe_id = updated_eval.id.into_inner();
    state.shutdown.spawn(async move {
        record_phase_event(
            &pe_state.worker_db,
            PHASE_SUBJECT_EVALUATION,
            pe_id,
            i32::from(event_status) as i16,
            None,
            now,
        )
        .await;
    });

    updated_eval
}

/// Records an error-level `evaluation_message` row and transitions the evaluation status.
///
/// `source` identifies where the error originated - e.g. `"flake-prefetch"`,
/// `"nix-eval"`, `"nix-eval:packages.x86_64-linux.hello"`, `"db-insert"`.
pub async fn update_evaluation_status_with_error(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
    error_message: String,
    source: Option<String>,
) -> MEvaluation {
    // If the evaluation is already in a terminal state (e.g. it was
    // aborted while we were running), don't record a spurious error or
    // overwrite the status - just return the current row.
    if matches!(
        evaluation.status,
        EvaluationStatus::Aborted | EvaluationStatus::Failed | EvaluationStatus::Completed
    ) {
        return evaluation;
    }

    debug!(evaluation_id = %evaluation.id, status = ?status, error = %error_message, ?source, "Updating evaluation status with error");

    let msg = AEvaluationMessage {
        id: Set(EvaluationMessageId::now_v7()),
        evaluation: Set(evaluation.id),
        level: Set(MessageLevel::Error),
        message: Set(error_message),
        source: Set(source),
        created_at: Set(crate::types::now()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&state.worker_db).await {
        error!(error = %e, evaluation_id = %evaluation.id, "Failed to insert evaluation_message");
    }

    update_evaluation_status(state, evaluation, status).await
}

/// Inserts a single `evaluation_message` row without changing the evaluation status.
///
/// Use for partial failures (e.g. one attr path failed to evaluate) where the
/// evaluation as a whole continues.
pub async fn record_evaluation_message(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    level: MessageLevel,
    message: String,
    source: Option<String>,
) {
    let msg = AEvaluationMessage {
        id: Set(EvaluationMessageId::now_v7()),
        evaluation: Set(evaluation_id),
        level: Set(level),
        message: Set(message),
        source: Set(source),
        created_at: Set(crate::types::now()),
    };
    if let Err(e) = EEvaluationMessage::insert(msg).exec(&state.worker_db).await {
        error!(error = %e, %evaluation_id, "Failed to insert evaluation_message");
    }
}

pub async fn abort_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    if evaluation.status == EvaluationStatus::Completed {
        return;
    }

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .filter(
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building)),
        )
        .all(&state.worker_db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to query builds for evaluation abort");
            return;
        }
    };

    for build in builds {
        if build.via.is_some() {
            // Follower: aborting it does not interrupt the leader's work in
            // another evaluation. Clear `via` so the eventual leader-completion
            // sweep skips it, then mark Aborted.
            abort_follower(&state, build).await;
            continue;
        }

        // Leader (or plain build).
        let has_followers = match EBuild::find()
            .filter(CBuild::Via.eq(build.id))
            .one(&state.worker_db)
            .await
        {
            Ok(opt) => opt.is_some(),
            Err(e) => {
                error!(error = %e, build_id = %build.id, "Failed to query followers for abort");
                false
            }
        };

        if has_followers && build.status == BuildStatus::Building {
            // Already running on a worker - let it finish so followers in
            // other (non-aborted) evaluations get the result.
            continue;
        }

        if has_followers && matches!(build.status, BuildStatus::Queued | BuildStatus::Created) {
            // Hand off leadership before aborting.
            if let Err(e) = reelect_leader(&state, &build).await {
                error!(error = %e, build_id = %build.id, "Failed to re-elect leader on abort");
            }
        }

        update_build_status(Arc::clone(&state), build, BuildStatus::Aborted).await;
    }

    update_evaluation_status(state, evaluation, EvaluationStatus::Aborted).await;
}

async fn abort_follower(state: &Arc<ServerState>, build: MBuild) {
    let mut active: ABuild = build.clone().into_active_model();
    active.via = Set(None);
    if let Err(e) = active.update(&state.worker_db).await {
        error!(error = %e, build_id = %build.id, "Failed to clear via on follower abort");
        return;
    }
    let reloaded = match EBuild::find_by_id(build.id).one(&state.worker_db).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to reload follower for abort");
            return;
        }
    };
    update_build_status(Arc::clone(state), reloaded, BuildStatus::Aborted).await;
}

/// Promote one same-org follower of `leader` to be the new leader.
/// Cross-org followers have their `via` cleared (made independent).
/// No-op if no followers exist.
pub(crate) async fn reelect_leader(
    state: &Arc<ServerState>,
    leader: &MBuild,
) -> Result<(), sea_orm::DbErr> {
    use entity::derivation::{Column as CDerivation, Entity as EDerivation};

    let leader_org = EDerivation::find_by_id(leader.derivation)
        .one(&state.worker_db)
        .await?
        .map(|d| d.organization);

    let all_followers = EBuild::find()
        .filter(CBuild::Via.eq(leader.id))
        .all(&state.worker_db)
        .await?;
    if all_followers.is_empty() {
        return Ok(());
    }

    let follower_drv_ids: Vec<DerivationId> = all_followers.iter().map(|f| f.derivation).collect();
    let drv_org: std::collections::HashMap<DerivationId, OrganizationId> =
        crate::db::fetch_in_chunks(&follower_drv_ids, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Id.is_in(chunk))
                .all(&state.worker_db)
                .await
        })
        .await?
        .into_iter()
        .map(|d| (d.id, d.organization))
        .collect();

    let mut same_org: Vec<MBuild> = Vec::new();
    let mut cross_org: Vec<MBuild> = Vec::new();
    for f in all_followers {
        let org = drv_org.get(&f.derivation).copied();
        if org == leader_org && org.is_some() {
            same_org.push(f);
        } else {
            cross_org.push(f);
        }
    }

    fn rank(s: BuildStatus) -> u8 {
        match s {
            BuildStatus::Building => 2,
            BuildStatus::Queued => 1,
            _ => 0,
        }
    }
    same_org.sort_by(|a, b| {
        rank(b.status)
            .cmp(&rank(a.status))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    if let Some(new_leader) = same_org.first().cloned() {
        let mut active: ABuild = new_leader.clone().into_active_model();
        active.via = Set(None);
        active.update(&state.worker_db).await?;

        let same_org_remaining_ids: Vec<BuildId> = same_org.iter().skip(1).map(|f| f.id).collect();
        crate::db::for_each_chunk(&same_org_remaining_ids, |chunk| async move {
            EBuild::update_many()
                .col_expr(CBuild::Via, sea_orm::sea_query::Expr::value(new_leader.id))
                .filter(CBuild::Id.is_in(chunk))
                .exec(&state.worker_db)
                .await
        })
        .await?;

        let cross_org_ids: Vec<BuildId> = cross_org.iter().map(|f| f.id).collect();
        crate::db::for_each_chunk(&cross_org_ids, |chunk| async move {
            EBuild::update_many()
                .col_expr(
                    CBuild::Via,
                    sea_orm::sea_query::Expr::value(Option::<BuildId>::None),
                )
                .filter(CBuild::Id.is_in(chunk))
                .exec(&state.worker_db)
                .await
        })
        .await?;

        debug!(
            old_leader = %leader.id,
            new_leader = %new_leader.id,
            cross_org_orphaned = cross_org.len(),
            "re-elected build leader (same-org), cross-org followers made independent"
        );
        return Ok(());
    }

    let cross_org_ids: Vec<BuildId> = cross_org.iter().map(|f| f.id).collect();
    if !cross_org_ids.is_empty() {
        crate::db::for_each_chunk(&cross_org_ids, |chunk| async move {
            EBuild::update_many()
                .col_expr(
                    CBuild::Via,
                    sea_orm::sea_query::Expr::value(Option::<BuildId>::None),
                )
                .filter(CBuild::Id.is_in(chunk))
                .exec(&state.worker_db)
                .await
        })
        .await?;
        debug!(
            old_leader = %leader.id,
            orphaned = cross_org.len(),
            "leader aborted with no same-org followers; cross-org followers made independent"
        );
    }
    Ok(())
}

/// For each derivation in `drv_ids`, return the id of the leader build whose
/// result a new build for that derivation should follow.
///
/// First checks for an in-flight build within `inserting_org`. When no
/// same-org candidate exists for a drv, consults cache-connected organisations
/// via [`cache_reach::writer_orgs_reachable_from`] and picks the most-advanced
/// active build (tie-break: oldest `created_at`).
///
/// Drvs with no active build are omitted from the result.
pub async fn find_active_leaders<C: ConnectionTrait>(
    db: &C,
    inserting_org: OrganizationId,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, BuildId>, sea_orm::DbErr> {
    if drv_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // ── Same-org pass ────────────────────────────────────────────────────
    let same_org_rows = crate::db::fetch_in_chunks(drv_ids, |chunk| async move {
        EBuild::find()
            .filter(CBuild::Derivation.is_in(chunk))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .all(db)
            .await
    })
    .await?;

    let mut out: HashMap<DerivationId, BuildId> = HashMap::new();
    for b in same_org_rows {
        let head = b.via.unwrap_or(b.id);
        out.entry(b.derivation)
            .and_modify(|cur| {
                if b.via.is_none() {
                    *cur = b.id;
                }
            })
            .or_insert(head);
    }

    let unmatched: Vec<DerivationId> = drv_ids
        .iter()
        .copied()
        .filter(|d| !out.contains_key(d))
        .collect();
    if unmatched.is_empty() {
        return Ok(out);
    }

    // ── Cross-org pass ───────────────────────────────────────────────────
    use entity::derivation::{Column as CDerivation, Entity as EDerivation};

    let inserting_drv_rows = crate::db::fetch_in_chunks(&unmatched, |chunk| async move {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await?;
    let mut path_to_drv: HashMap<String, DerivationId> = HashMap::new();
    let mut drv_hashes: Vec<String> = Vec::new();
    for d in &inserting_drv_rows {
        path_to_drv.insert(d.drv_path(), d.id);
        drv_hashes.push(d.hash.clone());
    }
    if drv_hashes.is_empty() {
        return Ok(out);
    }

    let mut reachable =
        crate::db::cache_reach::writer_orgs_reachable_from(db, inserting_org).await?;
    reachable.remove(&inserting_org);
    if reachable.is_empty() {
        return Ok(out);
    }

    let reachable_orgs: Vec<_> = reachable.into_iter().collect();
    let candidate_drvs = crate::db::fetch_in_chunks(&drv_hashes, |chunk| {
        let reachable_orgs = reachable_orgs.clone();
        async move {
            EDerivation::find()
                .filter(CDerivation::Hash.is_in(chunk))
                .filter(CDerivation::Organization.is_in(reachable_orgs))
                .all(db)
                .await
        }
    })
    .await?;
    if candidate_drvs.is_empty() {
        return Ok(out);
    }
    let candidate_drv_ids: Vec<DerivationId> = candidate_drvs.iter().map(|d| d.id).collect();
    let leader_drv_to_path: HashMap<DerivationId, String> = candidate_drvs
        .into_iter()
        .map(|d| (d.id, d.drv_path()))
        .collect();

    let candidate_builds = crate::db::fetch_in_chunks(&candidate_drv_ids, |chunk| async move {
        EBuild::find()
            .filter(CBuild::Derivation.is_in(chunk))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ]))
            .filter(CBuild::Via.is_null())
            .filter(CBuild::ExternalCached.eq(false))
            .all(db)
            .await
    })
    .await?;

    fn status_rank(s: BuildStatus) -> u8 {
        match s {
            BuildStatus::Building => 2,
            BuildStatus::Queued => 1,
            _ => 0,
        }
    }
    let mut best_by_path: HashMap<String, MBuild> = HashMap::new();
    for b in candidate_builds {
        let Some(path) = leader_drv_to_path.get(&b.derivation).cloned() else {
            continue;
        };
        match best_by_path.get(&path) {
            Some(cur) => {
                let cur_rank = status_rank(cur.status);
                let new_rank = status_rank(b.status);
                if new_rank > cur_rank || (new_rank == cur_rank && b.created_at < cur.created_at) {
                    best_by_path.insert(path, b);
                }
            }
            None => {
                best_by_path.insert(path, b);
            }
        }
    }

    for (path, b) in best_by_path {
        if let Some(&local_drv_id) = path_to_drv.get(&path) {
            out.insert(local_drv_id, b.id);
        }
    }

    Ok(out)
}

pub async fn dispatch_build_event_for_status(
    state: &Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) {
    let event = match status {
        BuildStatus::Queued => "build.queued",
        BuildStatus::Building => "build.started",
        BuildStatus::Completed => "build.completed",
        BuildStatus::FailedPermanent => "build.failed",
        BuildStatus::FailedTimeout => "build.failed",
        BuildStatus::FailedTransient => "build.failed_transient",
        BuildStatus::Substituted => "build.substituted",
        BuildStatus::Created | BuildStatus::Aborted | BuildStatus::DependencyFailed => return,
    };

    let evaluation = match EEvaluation::find_by_id(build.evaluation)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(e)) => e,
        Ok(None) => {
            warn!(evaluation_id = %build.evaluation, "Evaluation not found for action dispatch");
            return;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %build.evaluation, "DB error looking up evaluation for action dispatch");
            return;
        }
    };

    let project_id = match evaluation.project {
        Some(id) => id,
        None => return,
    };

    let derivation_path = EDerivation::find_by_id(build.derivation)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()
        .map(|d| d.store_path());

    let payload = serde_json::json!({
        "build_id": build.id,
        "evaluation_id": build.evaluation,
        "derivation_path": derivation_path,
        "status": event,
    });

    dispatch_build_event(state, project_id, event, payload).await;
}

async fn dispatch_evaluation_event_for_status(
    state: &Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) {
    let event = match status {
        EvaluationStatus::Queued => "evaluation.queued",
        EvaluationStatus::Fetching
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation => "evaluation.started",
        EvaluationStatus::Building => "evaluation.building",
        EvaluationStatus::Waiting => "evaluation.waiting",
        EvaluationStatus::Completed => "evaluation.completed",
        EvaluationStatus::Failed => "evaluation.failed",
        EvaluationStatus::Aborted => "evaluation.aborted",
    };

    let project_id = match evaluation.project {
        Some(id) => id,
        None => return,
    };

    let payload = serde_json::json!({
        "evaluation_id": evaluation.id,
        "project_id": evaluation.project,
        "repository": evaluation.repository,
        "status": event,
    });

    dispatch_evaluation_event(state, project_id, event, payload).await;

    react_to_source_comment_on_terminal(state, project_id, &evaluation, status).await;
}

/// If the evaluation has a `source_comment` (set by the `/gradient run` or
/// `/gradient approve` PR-comment pipeline) and the status is terminal, post a
/// thumbs-up / thumbs-down reaction on that comment via the project's
/// configured reporter. Best-effort: failures are logged and swallowed.
async fn react_to_source_comment_on_terminal(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    evaluation: &MEvaluation,
    status: EvaluationStatus,
) {
    use crate::ci::{ReactionKind, actions::reporter_for_project};

    let kind = match status {
        EvaluationStatus::Completed => ReactionKind::ThumbsUp,
        EvaluationStatus::Failed | EvaluationStatus::Aborted => ReactionKind::ThumbsDown,
        _ => return,
    };
    let Some(raw) = evaluation.source_comment.as_ref() else {
        return;
    };
    let Some(target) = parse_source_comment(raw) else {
        warn!(
            evaluation_id = %evaluation.id,
            "evaluation.source_comment present but malformed; skipping reaction"
        );
        return;
    };
    let reporter = match reporter_for_project(state, project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %project_id, "resolving reporter for terminal-status reaction");
            return;
        }
    };
    if let Err(e) = reporter.add_reaction(&target, kind).await {
        warn!(error = %e, %project_id, ?kind, "/gradient terminal reaction post failed");
    }
}

fn parse_source_comment(value: &serde_json::Value) -> Option<crate::ci::ReactionTarget> {
    let owner = value.get("owner")?.as_str()?.to_string();
    let repo = value.get("repo")?.as_str()?.to_string();
    let pr_number = value.get("pr_number")?.as_u64()?;
    let comment_id = value.get("comment_id")?.as_i64()?;
    Some(crate::ci::ReactionTarget {
        owner,
        repo,
        pr_number,
        comment_id,
    })
}

#[cfg(test)]
mod reelect_leader_tests {
    use super::*;
    use entity::build::{BuildStatus, Model as MBuild};
    use entity::derivation::Model as MDerivation;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org(n: u8) -> OrganizationId {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        OrganizationId::new(Uuid::from_bytes(bytes))
    }
    fn did(n: u8) -> DerivationId {
        let mut bytes = [0u8; 16];
        bytes[13] = n;
        DerivationId::new(Uuid::from_bytes(bytes))
    }
    fn bid(n: u8) -> BuildId {
        let mut bytes = [0u8; 16];
        bytes[12] = n;
        BuildId::new(Uuid::from_bytes(bytes))
    }

    fn build(id: BuildId, drv: DerivationId, via: Option<BuildId>, status: BuildStatus) -> MBuild {
        MBuild {
            id,
            evaluation: EvaluationId::now_v7(),
            derivation: drv,
            status,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via,
            external_cached: false,
            attempt: 0,
            timeout_secs: None,
            max_silent_secs: None,
            prefer_local_build: false,
            created_at: chrono::NaiveDateTime::default(),
            updated_at: chrono::NaiveDateTime::default(),
            queued_at: None,
            ready_at: None,
            dispatched_at: None,
            build_started_at: None,
            build_finished_at: None,
        }
    }

    fn drv_row(id: DerivationId, owner: OrganizationId) -> MDerivation {
        MDerivation {
            id,
            organization: owner,
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "x".into(),
            architecture: "x86_64-linux".into(),
            created_at: chrono::NaiveDateTime::default(),
            ..Default::default()
        }
    }

    fn make_state(db: sea_orm::DatabaseConnection) -> std::sync::Arc<crate::types::ServerState> {
        use crate::storage::{EmailSender, LogStorage, NarStore};
        use crate::types::{RuntimeConfig, SecretString, WebDb, WorkerDb};
        use futures::future::BoxFuture;

        #[derive(Debug)]
        struct NoopLog;
        impl LogStorage for NoopLog {
            fn append<'a>(
                &'a self,
                _: entity::ids::BuildId,
                _: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn read<'a>(
                &'a self,
                _: entity::ids::BuildId,
            ) -> BoxFuture<'a, anyhow::Result<String>> {
                Box::pin(async { Ok(String::new()) })
            }
            fn delete<'a>(&'a self, _: entity::ids::BuildId) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn list_logs<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<Vec<entity::ids::BuildId>>> {
                Box::pin(async { Ok(Vec::new()) })
            }
            fn write_chunk<'a>(
                &'a self,
                _: entity::ids::BuildId,
                _: u32,
                _: &'a [u8],
            ) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn read_chunk<'a>(
                &'a self,
                _: entity::ids::BuildId,
                _: u32,
            ) -> BoxFuture<'a, anyhow::Result<Vec<u8>>> {
                Box::pin(async { anyhow::bail!("no chunk") })
            }
            fn delete_chunks<'a>(
                &'a self,
                _: entity::ids::BuildId,
            ) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { Ok(()) })
            }
        }

        #[derive(Debug)]
        struct NoopEmail;
        #[async_trait::async_trait]
        impl EmailSender for NoopEmail {
            fn is_enabled(&self) -> bool {
                false
            }
            async fn send_verification_email(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn send_password_reset_email(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn send_action_mail(
                &self,
                _: &[String],
                _: &str,
                _: &str,
            ) -> anyhow::Result<crate::storage::email::MailDeliveryResult> {
                Ok(crate::storage::email::MailDeliveryResult {
                    status_code: 0,
                    server_response: String::new(),
                })
            }
        }

        let cli = crate::types::Cli {
            logging: crate::types::LoggingArgs::default(),
            server: crate::types::ServerArgs::default(),
            database: crate::types::DatabaseArgs::default(),
            eval: crate::types::EvalArgs::default(),
            storage: crate::types::StorageArgs {
                base_path: "/tmp/gradient-test".into(),
                ..Default::default()
            },
            secrets: crate::types::SecretsArgs {
                crypt_secret_file: "test-secret".into(),
                jwt_secret_file: "test-jwt".into(),
            },
            limits: crate::types::LimitsArgs::default(),
            registration: crate::types::RegistrationArgs::default(),
            proto: crate::types::ProtoArgs::default(),
            oidc: crate::types::OidcArgs::default(),
            email: crate::types::EmailArgs::default(),
            s3: crate::types::S3Args::default(),
            github_app: crate::types::GitHubAppArgs::default(),
            metrics: crate::types::MetricsArgs::default(),
            network: crate::types::NetworkArgs::default(),
        };
        let config = std::sync::Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
        let nar_storage = NarStore::local(&config.storage.base_path).expect("nar store");
        std::sync::Arc::new(crate::types::ServerState {
            web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            worker_db: WorkerDb::new(db),
            config,
            log_storage: std::sync::Arc::new(NoopLog),
            email: std::sync::Arc::new(NoopEmail) as std::sync::Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            pending_credentials: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            http: crate::http::build_client().expect("http client"),
            shutdown: crate::shutdown::Shutdown::new(),
            jwt_secret: SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
        })
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// Same-org follower is promoted to leader; cross-org follower's via is cleared.
    #[test]
    fn promotes_same_org_and_orphans_cross_org_follower() {
        run(async {
            let leader_drv = did(1);
            let same_org_drv = did(2);
            let cross_org_drv = did(3);
            let leader = build(bid(10), leader_drv, None, BuildStatus::Queued);
            let same_org_follower =
                build(bid(11), same_org_drv, Some(leader.id), BuildStatus::Created);
            let cross_org_follower = build(
                bid(12),
                cross_org_drv,
                Some(leader.id),
                BuildStatus::Created,
            );

            // Query sequence:
            //   1. EDerivation::find_by_id(leader.derivation) → drv with org(1)
            //   2. EBuild::find().filter(Via = leader.id) → [same_org_follower, cross_org_follower]
            //   3. EDerivation::find().filter(Id IN follower_drv_ids) → org map
            //   4. active.update() for promotion (Postgres RETURNING → query_results)
            //   5. EBuild::update_many (clear via for cross_org_follower → exec_results)
            // No "remaining same-org" update_many: only one same-org follower → skip(1) is empty.
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![drv_row(leader_drv, org(1))]])
                .append_query_results([vec![same_org_follower.clone(), cross_org_follower.clone()]])
                .append_query_results([vec![
                    drv_row(same_org_drv, org(1)),
                    drv_row(cross_org_drv, org(2)),
                ]])
                .append_query_results([vec![same_org_follower.clone()]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection();

            let state = make_state(db);
            reelect_leader(&state, &leader).await.expect("reelect ok");
        });
    }

    /// Leader has only cross-org followers: every follower's via is cleared.
    #[test]
    fn all_cross_org_followers_orphaned_when_no_same_org() {
        run(async {
            let leader_drv = did(1);
            let foll_drv_b = did(2);
            let foll_drv_c = did(3);
            let leader = build(bid(20), leader_drv, None, BuildStatus::Queued);
            let f1 = build(bid(21), foll_drv_b, Some(leader.id), BuildStatus::Created);
            let f2 = build(bid(22), foll_drv_c, Some(leader.id), BuildStatus::Created);

            // Query sequence:
            //   1. EDerivation::find_by_id(leader.derivation) → drv with org(1)
            //   2. EBuild::find().filter(Via = leader.id) → [f1, f2]
            //   3. EDerivation::find().filter(Id IN follower_drv_ids) → org map (all cross-org)
            //   4. EBuild::update_many (clear via for all cross-org → exec_results)
            // No same-org candidates, so no active.update() promotion.
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![drv_row(leader_drv, org(1))]])
                .append_query_results([vec![f1.clone(), f2.clone()]])
                .append_query_results([vec![
                    drv_row(foll_drv_b, org(2)),
                    drv_row(foll_drv_c, org(3)),
                ]])
                .append_exec_results([sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                }])
                .into_connection();

            let state = make_state(db);
            reelect_leader(&state, &leader).await.expect("reelect ok");
        });
    }
}

#[cfg(test)]
mod find_active_leaders_tests {
    use super::*;
    use entity::build::{BuildStatus, Model as MBuild};
    use entity::cache_upstream::Model as MCacheUpstream;
    use entity::derivation::Model as MDerivation;
    use entity::ids::{BuildId, CacheId, DerivationId, OrganizationCacheId, OrganizationId};
    use entity::organization_cache::{CacheSubscriptionMode, Model as MOrganizationCache};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use uuid::Uuid;

    fn org(n: u8) -> OrganizationId {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        OrganizationId::new(Uuid::from_bytes(bytes))
    }
    fn cid(n: u8) -> CacheId {
        let mut bytes = [0u8; 16];
        bytes[14] = n;
        CacheId::new(Uuid::from_bytes(bytes))
    }
    fn did(n: u8) -> DerivationId {
        let mut bytes = [0u8; 16];
        bytes[13] = n;
        DerivationId::new(Uuid::from_bytes(bytes))
    }
    fn bid(n: u8) -> BuildId {
        let mut bytes = [0u8; 16];
        bytes[12] = n;
        BuildId::new(Uuid::from_bytes(bytes))
    }

    fn build(
        id: BuildId,
        drv: DerivationId,
        status: BuildStatus,
        external_cached: bool,
        offset_secs: i64,
    ) -> MBuild {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + chrono::Duration::seconds(offset_secs);
        MBuild {
            id,
            evaluation: entity::ids::EvaluationId::now_v7(),
            derivation: drv,
            status,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached,
            attempt: 0,
            timeout_secs: None,
            max_silent_secs: None,
            prefer_local_build: false,
            created_at: t,
            updated_at: t,
            queued_at: None,
            ready_at: None,
            dispatched_at: None,
            build_started_at: None,
            build_finished_at: None,
        }
    }

    fn drv_row(id: DerivationId, owner: OrganizationId, _path: &str) -> MDerivation {
        MDerivation {
            id,
            organization: owner,
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "x".into(),
            architecture: "x86_64-linux".into(),
            created_at: chrono::NaiveDateTime::default(),
            ..Default::default()
        }
    }

    fn run<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn chunks_large_id_lists_under_postgres_param_cap() {
        run(async {
            // Postgres rejects any statement binding more than 65535 params. A
            // large monorepo restart funnels tens of thousands of derivation ids
            // through `find_active_leaders`; without chunking the `is_in`
            // overflows and `/evaluate` 500s ("too many arguments for query").
            const PG_MAX_PARAMS: usize = 65_535;
            let drv_ids: Vec<DerivationId> = (1..=70_000u128)
                .map(|i| DerivationId::new(Uuid::from_u128(i)))
                .collect();

            // Same-org pass and cross-org derivation lookup both return nothing,
            // so `drv_hashes` is empty and the function returns early. Empty
            // result sets satisfy every chunked query regardless of model type.
            let mut db = MockDatabase::new(DatabaseBackend::Postgres);
            for _ in 0..64 {
                db = db.append_query_results([Vec::<MBuild>::new()]);
            }
            let db = db.into_connection();

            let got = find_active_leaders(&db, org(1), &drv_ids).await.unwrap();
            assert!(got.is_empty());

            for txn in db.into_transaction_log() {
                for stmt in txn.statements() {
                    let n = stmt.values.as_ref().map(|v| v.0.len()).unwrap_or(0);
                    assert!(
                        n <= PG_MAX_PARAMS,
                        "statement bound {n} params over the {PG_MAX_PARAMS} cap: {}",
                        stmt.sql
                    );
                }
            }
        });
    }

    #[test]
    fn cross_org_match_when_no_same_org_candidate() {
        run(async {
            let drv_b = did(2);
            let drv_a = did(1);
            let leader_build = bid(10);

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MBuild>::new()])
                .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(2),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadOnly,
                }]])
                .append_query_results([Vec::<MCacheUpstream>::new()])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(1),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadWrite,
                }]])
                .append_query_results([vec![drv_row(drv_a, org(1), "/nix/store/x.drv")]])
                .append_query_results([vec![build(
                    leader_build,
                    drv_a,
                    BuildStatus::Building,
                    false,
                    0,
                )]])
                .into_connection();

            let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
            assert_eq!(got.get(&drv_b), Some(&leader_build), "got: {:?}", got);
        });
    }

    #[test]
    fn cross_org_tie_break_most_advanced_then_oldest() {
        run(async {
            let drv_b = did(2);
            let drv_a = did(1);
            let drv_c = did(3);
            let queued_old = bid(20);
            let building_new = bid(21);

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MBuild>::new()])
                .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(2),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadOnly,
                }]])
                .append_query_results([Vec::<MCacheUpstream>::new()])
                .append_query_results([vec![
                    MOrganizationCache {
                        id: OrganizationCacheId::now_v7(),
                        organization: org(1),
                        cache: cid(1),
                        mode: CacheSubscriptionMode::ReadWrite,
                    },
                    MOrganizationCache {
                        id: OrganizationCacheId::now_v7(),
                        organization: org(3),
                        cache: cid(1),
                        mode: CacheSubscriptionMode::ReadWrite,
                    },
                ]])
                .append_query_results([vec![
                    drv_row(drv_a, org(1), "/nix/store/x.drv"),
                    drv_row(drv_c, org(3), "/nix/store/x.drv"),
                ]])
                .append_query_results([vec![
                    build(queued_old, drv_a, BuildStatus::Queued, false, 0),
                    build(building_new, drv_c, BuildStatus::Building, false, 60),
                ]])
                .into_connection();

            let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
            assert_eq!(got.get(&drv_b), Some(&building_new), "got: {:?}", got);
        });
    }

    #[test]
    fn same_org_preferred_over_cross_org() {
        run(async {
            let drv_b = did(2);
            let same_org_build = bid(30);

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![build(
                    same_org_build,
                    drv_b,
                    BuildStatus::Queued,
                    false,
                    0,
                )]])
                .into_connection();

            let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
            assert_eq!(got.get(&drv_b), Some(&same_org_build));
        });
    }

    #[test]
    fn cross_org_external_cached_candidate_skipped() {
        run(async {
            let drv_b = did(2);
            let drv_a = did(1);

            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<MBuild>::new()])
                .append_query_results([vec![drv_row(drv_b, org(2), "/nix/store/x.drv")]])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(2),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadOnly,
                }]])
                .append_query_results([Vec::<MCacheUpstream>::new()])
                .append_query_results([vec![MOrganizationCache {
                    id: OrganizationCacheId::now_v7(),
                    organization: org(1),
                    cache: cid(1),
                    mode: CacheSubscriptionMode::ReadWrite,
                }]])
                .append_query_results([vec![drv_row(drv_a, org(1), "/nix/store/x.drv")]])
                .append_query_results([Vec::<MBuild>::new()])
                .into_connection();

            let got = find_active_leaders(&db, org(2), &[drv_b]).await.unwrap();
            assert!(!got.contains_key(&drv_b), "external_cached must be skipped");
        });
    }
}
