/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_status::update_evaluation_status;
use super::leader_election::reelect_leader;
use super::logging::{PHASE_SUBJECT_BUILD, finalize_build_log, record_phase_events};
use crate::dep_closure::reconcile_eval_dep_counts;
use crate::state_machine::EvalStateMachine;
use crate::{DbContext, fetch_in_chunks, for_each_chunk};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{Column as CAttempt, Entity as EAttempt};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use std::collections::HashSet;
use tracing::error;

/// Abort an evaluation's in-flight builds in bulk. Aborting an evaluation with
/// tens of thousands of builds used to walk them one at a time (a follower
/// lookup, a row update, and several spawned side-effects per build), so the
/// request blocked for the whole closure. This resolves the whole set with a
/// handful of set-based statements: one follower-leader query, one status
/// update, one attempt stamp, and one dependency-count reconcile.
pub async fn abort_evaluation(ctx: &DbContext, evaluation: MEvaluation) {
    if EvalStateMachine::is_terminal(&evaluation.status) {
        return;
    }

    // Park the evaluation before touching its builds: the dispatcher's queue
    // finder skips Waiting evaluations, so this stops new builds being handed
    // to workers while we abort. Direct update (no status-change side effects);
    // the terminal transition to Aborted below fires those.
    gate_evaluation_aborting(ctx, evaluation.id).await;

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .filter(CBuild::Status.is_in([
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ]))
        .all(&ctx.worker_db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation.id, "Failed to query builds for evaluation abort");
            return;
        }
    };

    if !builds.is_empty() {
        abort_builds(ctx, &evaluation, builds).await;
    }

    update_evaluation_status(ctx, evaluation, EvaluationStatus::Aborted).await;
}

/// Park the evaluation as `Waiting` with the `Aborting` reason so the dispatcher
/// stops handing out its builds. A direct filtered update: the eventual
/// transition to `Aborted` carries the user-facing status-change side effects.
async fn gate_evaluation_aborting(ctx: &DbContext, evaluation_id: EvaluationId) {
    let res = EEvaluation::update_many()
        .col_expr(
            CEvaluation::Status,
            Expr::value(EvaluationStatus::Waiting),
        )
        .col_expr(
            CEvaluation::WaitingReason,
            Expr::value(WaitingReason::Aborting.to_json()),
        )
        .col_expr(CEvaluation::UpdatedAt, Expr::value(gradient_types::now()))
        .filter(CEvaluation::Id.eq(evaluation_id))
        .filter(CEvaluation::Status.is_not_in([
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ]))
        .exec(&ctx.worker_db)
        .await;

    if let Err(e) = res {
        error!(error = %e, %evaluation_id, "Failed to park evaluation for abort");
    }
}

async fn abort_builds(ctx: &DbContext, evaluation: &MEvaluation, builds: Vec<MBuild>) {
    let build_ids: Vec<BuildId> = builds.iter().map(|b| b.id).collect();
    let leaders = leader_ids_with_followers(ctx, &build_ids).await;
    let plan = partition_for_abort(builds, &leaders);

    // Hand leadership off so followers in other, non-aborted evaluations still
    // get a result. Always a small set, so a per-build call is fine.
    for leader in &plan.reelect {
        if let Err(e) = reelect_leader(ctx, leader).await {
            error!(error = %e, build_id = %leader.id, "Failed to re-elect leader on abort");
        }
    }

    let abort_ids = plan.abort_ids();
    if abort_ids.is_empty() {
        return;
    }

    let now = gradient_types::now();

    // Mark Aborted and detach followers from their (external) leader in one pass.
    if let Err(e) = for_each_chunk(&abort_ids, |chunk| async move {
        EBuild::update_many()
            .col_expr(CBuild::Status, Expr::value(BuildStatus::Aborted))
            .col_expr(CBuild::Via, Expr::value(Option::<BuildId>::None))
            .col_expr(CBuild::UpdatedAt, Expr::value(now))
            .filter(CBuild::Id.is_in(chunk))
            .exec(&ctx.worker_db)
            .await
    })
    .await
    {
        error!(error = %e, evaluation_id = %evaluation.id, "Failed to bulk-abort builds");
        return;
    }

    // Only executing builds have an open attempt to stamp finished.
    let building_ids = plan.building_ids();
    if !building_ids.is_empty()
        && let Err(e) = for_each_chunk(&building_ids, |chunk| async move {
            EAttempt::update_many()
                .col_expr(CAttempt::BuildFinishedAt, Expr::value(Some(now)))
                .filter(CAttempt::Build.is_in(chunk))
                .filter(CAttempt::BuildFinishedAt.is_null())
                .exec(&ctx.worker_db)
                .await
        })
        .await
    {
        error!(error = %e, evaluation_id = %evaluation.id, "Failed to stamp aborted attempts");
    }

    // One reconcile replaces the per-build dep-count delta (#383).
    if let Err(e) = reconcile_eval_dep_counts(&ctx.worker_db, evaluation.id).await {
        error!(error = %e, evaluation_id = %evaluation.id, "Failed to reconcile dep counts after abort");
    }

    let pe_ids: Vec<uuid::Uuid> = abort_ids.iter().map(|&id| id.into_inner()).collect();
    record_phase_events(
        &ctx.worker_db,
        PHASE_SUBJECT_BUILD,
        &pe_ids,
        i32::from(BuildStatus::Aborted) as i16,
        now,
    )
    .await;

    finalize_aborted_logs(ctx, &building_ids).await;

    // One coarse ping; live subscribers refetch their own scope.
    let _ = ctx
        .board_events
        .send(gradient_types::BoardEvent::EvaluationProgress {
            project: evaluation.project.map(|p| p.into_inner()),
            evaluation_id: evaluation.id.into_inner(),
        });
}

/// The ids of this evaluation's builds that other builds follow (the `via`
/// targets). One query replaces the former per-leader follower lookup.
async fn leader_ids_with_followers(ctx: &DbContext, build_ids: &[BuildId]) -> HashSet<BuildId> {
    let rows = fetch_in_chunks(build_ids, |chunk| async move {
        EBuild::find()
            .select_only()
            .column(CBuild::Via)
            .distinct()
            .filter(CBuild::Via.is_in(chunk))
            .into_tuple::<Option<BuildId>>()
            .all(&ctx.worker_db)
            .await
    })
    .await;

    match rows {
        Ok(rows) => rows.into_iter().flatten().collect(),
        Err(e) => {
            error!(error = %e, "Failed to query build followers for abort");
            HashSet::new()
        }
    }
}

/// Compress the build log of each executing build that was aborted. Spawned so
/// log I/O never blocks the abort; Created/Queued builds never produced a log.
async fn finalize_aborted_logs(ctx: &DbContext, building_ids: &[BuildId]) {
    if building_ids.is_empty() {
        return;
    }

    let attempts = crate::build_attempt::latest_attempts(&ctx.worker_db, building_ids)
        .await
        .unwrap_or_default();
    for &build_id in building_ids {
        let log_id = attempts
            .get(&build_id)
            .and_then(|a| a.log_id)
            .unwrap_or(build_id);
        let log_ctx = ctx.clone();
        ctx.shutdown.spawn(async move {
            finalize_build_log(&log_ctx, log_id).await;
        });
    }
}

/// What to do with each in-flight build when its evaluation is aborted.
struct AbortPlan {
    /// Leaders with followers that are still Queued/Created: hand leadership off
    /// before aborting. Always small.
    reelect: Vec<MBuild>,
    /// Every build to mark Aborted (followers, plain builds, and the re-elected
    /// leaders). Building leaders with followers are excluded so they keep
    /// running for dependent evaluations.
    abort: Vec<MBuild>,
}

impl AbortPlan {
    fn abort_ids(&self) -> Vec<BuildId> {
        self.abort.iter().map(|b| b.id).collect()
    }

    fn building_ids(&self) -> Vec<BuildId> {
        self.abort
            .iter()
            .filter(|b| b.status == BuildStatus::Building)
            .map(|b| b.id)
            .collect()
    }
}

fn partition_for_abort(builds: Vec<MBuild>, leaders_with_followers: &HashSet<BuildId>) -> AbortPlan {
    let mut reelect = Vec::new();
    let mut abort = Vec::new();

    for build in builds {
        if build.via.is_some() {
            abort.push(build);
            continue;
        }

        if leaders_with_followers.contains(&build.id) {
            match build.status {
                BuildStatus::Building => continue,
                BuildStatus::Queued | BuildStatus::Created => {
                    reelect.push(build.clone());
                    abort.push(build);
                }
                _ => abort.push(build),
            }
        } else {
            abort.push(build);
        }
    }

    AbortPlan { reelect, abort }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::{BuildId, DerivationId, EvaluationId};

    fn build(status: BuildStatus, via: Option<BuildId>) -> MBuild {
        MBuild {
            id: BuildId::now_v7(),
            evaluation: EvaluationId::now_v7(),
            derivation: DerivationId::now_v7(),
            status,
            via,
            ..Default::default()
        }
    }

    #[test]
    fn partition_skips_busy_leaders_reelects_idle_leaders_and_aborts_the_rest() {
        let plain = build(BuildStatus::Queued, None);
        let follower = build(BuildStatus::Building, Some(BuildId::now_v7()));
        let idle_leader = build(BuildStatus::Queued, None);
        let busy_leader = build(BuildStatus::Building, None);

        let leaders: HashSet<BuildId> = [idle_leader.id, busy_leader.id].into_iter().collect();
        let plan = partition_for_abort(
            vec![
                plain.clone(),
                follower.clone(),
                idle_leader.clone(),
                busy_leader.clone(),
            ],
            &leaders,
        );

        let abort_ids: HashSet<BuildId> = plan.abort_ids().into_iter().collect();
        assert!(abort_ids.contains(&plain.id));
        assert!(abort_ids.contains(&follower.id));
        assert!(abort_ids.contains(&idle_leader.id));
        assert!(
            !abort_ids.contains(&busy_leader.id),
            "a running leader with followers must keep running"
        );

        assert_eq!(plan.reelect.len(), 1);
        assert_eq!(plan.reelect[0].id, idle_leader.id);

        assert_eq!(
            plan.building_ids(),
            vec![follower.id],
            "only executing aborted builds need attempt/log finalization"
        );
    }
}
