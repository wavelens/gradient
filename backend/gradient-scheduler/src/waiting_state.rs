/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Reconciles in-flight evaluation status against the currently connected
//! worker pool (`Queued`/`Building` <-> `Waiting`), and self-heals a
//! graph-stuck evaluation.

use std::sync::Arc;

use anyhow::{Context, Result};

use gradient_core::ServerState;
use gradient_db::update_evaluation_status;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement, Value,
};
use tracing::{info, warn};

/// How long an evaluation must have sat `graph_stuck` before `.drv`-recovery
/// re-evaluates it, so a still-in-flight `.drv` upload isn't raced into a
/// needless re-eval. The graph-stuck reason is only rewritten on change, so a
/// stably-stuck eval's `updated_at` ages past this quickly.
const DRV_RECOVERY_GRACE_SECS: i64 = 120;

use crate::buildability::BuildabilityChecker;

/// Sweep every in-flight evaluation and reconcile its status against the
/// current set of connected workers, keyed on the eval's current state:
///
/// - **Pre-build** (`Queued`/`Fetching`/`EvaluatingFlake`/
///   `EvaluatingDerivation`): park to `Waiting` with an `EvalWorkers` reason
///   when no worker provides the capability the state needs - `fetch` for
///   `Fetching`, `eval` otherwise - regardless of any builds the eval has
///   already batched (so a mid-eval stall is caught, not skipped).
/// - **Build** (`Building`): flip `Building ↔ Waiting` from whether the pool
///   can satisfy any pending build's `(architecture, required_features)`.
/// - **Waiting**: recover via the reason it parked under - `EvalWorkers`
///   back to `Queued` once the capability returns, `Workers` back to
///   `Building` once buildable. `Approval`/`NoCache`/`CacheStorageFull` parks
///   are owned by other hooks and left untouched.
pub async fn reconcile_waiting_state(
    state: &Arc<ServerState>,
    worker_caps: &[(Vec<String>, Vec<String>)],
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
    draining: bool,
) -> Result<()> {
    let evals = EEvaluation::find()
        .filter(CEvaluation::Status.is_in(vec![
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ]))
        .all(&state.worker_db)
        .await
        .context("fetch in-flight evaluations")?;
    if evals.is_empty() {
        return Ok(());
    }

    // Draining: park every in-flight evaluation so the server can be stopped
    // safely. Dispatch is already gated, so parked evals stay put until
    // draining is disabled (recovered to `Queued` by the branch below).
    if draining {
        for eval in evals {
            let reason = eval
                .waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json);
            if eval.status == EvaluationStatus::Waiting
                && matches!(
                    reason,
                    Some(WaitingReason::Draining) | Some(WaitingReason::Aborting)
                )
            {
                continue;
            }

            let needs_status_change = eval.status != EvaluationStatus::Waiting;
            persist_waiting_reason(
                state,
                eval.id,
                &eval.waiting_reason,
                Some(&WaitingReason::Draining),
            )
            .await;

            if needs_status_change {
                info!(evaluation_id = %eval.id, from = ?eval.status, "parking evaluation: instance draining");
                update_evaluation_status(&state.db(), eval, EvaluationStatus::Waiting).await;
            }
        }

        return Ok(());
    }

    let connected_workers = worker_caps.len() as u32;

    for eval in evals {
        let reason = eval
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json);

        // Approval, no-cache and storage-full parks are owned by webhook +
        // cache hooks; an Aborting park is owned by the abort path. The
        // reconciler must not unpark any of them just because workers showed up.
        if eval.status == EvaluationStatus::Waiting
            && reason.as_ref().is_some_and(|r| {
                matches!(
                    r,
                    WaitingReason::Approval { .. }
                        | WaitingReason::NoCache
                        | WaitingReason::CacheStorageFull
                        | WaitingReason::Aborting
                )
            })
        {
            continue;
        }

        let outcome = match eval.status {
            EvaluationStatus::Waiting => match reason {
                Some(WaitingReason::EvalWorkers { capability, .. }) => Some(decide_eval_recovery(
                    capability,
                    eval_capable_workers,
                    fetch_capable_workers,
                    connected_workers,
                )),
                // Draining was disabled: resume from the queue and let the
                // next pass re-park to the appropriate capacity reason.
                Some(WaitingReason::Draining) => Some((EvaluationStatus::Queued, None)),
                // Build-phase park, or a legacy/untagged row: recover from the
                // pending builds, falling back to eval recovery when the eval
                // never produced any (a pre-build park predating EvalWorkers).
                _ => match build_phase_decision(state, eval.id, worker_caps).await? {
                    Some(pair) => Some(pair),
                    None => Some(decide_eval_recovery(
                        EvalCapability::Eval,
                        eval_capable_workers,
                        fetch_capable_workers,
                        connected_workers,
                    )),
                },
            },
            EvaluationStatus::Queued
            | EvaluationStatus::Fetching
            | EvaluationStatus::EvaluatingFlake
            | EvaluationStatus::EvaluatingDerivation => decide_pre_build_target(
                eval.status,
                eval_capable_workers,
                fetch_capable_workers,
                connected_workers,
            ),
            EvaluationStatus::Building => build_phase_decision(state, eval.id, worker_caps).await?,
            _ => None,
        };

        let Some((target, new_reason)) = outcome else {
            continue;
        };

        if eval.status != target {
            info!(
                evaluation_id = %eval.id,
                from = ?eval.status,
                to = ?target,
                workers = connected_workers,
                eval_workers = eval_capable_workers,
                fetch_workers = fetch_capable_workers,
                "reconciling evaluation waiting state"
            );
        }

        persist_waiting_reason(state, eval.id, &eval.waiting_reason, new_reason.as_ref()).await;

        if eval.status != target {
            update_evaluation_status(&state.db(), eval, target).await;
        }
    }

    Ok(())
}

/// Build-phase reconciliation for one evaluation: decide `Building` vs
/// `Waiting` from whether the connected pool can satisfy any of the eval's
/// pending anchors. Returns `None` when the eval has no pending anchor
/// (nothing to decide).
///
/// A `Waiting` verdict with an empty `unmet` set means the pool *can* build
/// every pending anchor yet none is dispatchable - the whole set is `Created`,
/// blocked behind the `closure_complete` gate with no in-flight build to fire
/// a promotion. `propagate_closure_complete` can't reach this (it runs on
/// completion events), so we self-heal here: [`attempt_graph_unstick`].
async fn build_phase_decision(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    worker_caps: &[(Vec<String>, Vec<String>)],
) -> Result<Option<(EvaluationStatus, Option<WaitingReason>)>> {
    let outcome = assess_buildability(state, evaluation_id, worker_caps).await?;

    if let Some((EvaluationStatus::Waiting, Some(WaitingReason::Workers { unmet, .. }))) = &outcome
        && unmet.is_empty()
    {
        return Ok(Some(
            attempt_graph_unstick(state, evaluation_id, worker_caps).await?,
        ));
    }

    Ok(outcome)
}

/// Decide `Building` vs `Waiting` for an eval's current pending anchors.
/// Returns `None` when nothing is pending (nothing to decide).
async fn assess_buildability(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    worker_caps: &[(Vec<String>, Vec<String>)],
) -> Result<Option<(EvaluationStatus, Option<WaitingReason>)>> {
    let pending = eval_pending_anchors(state, evaluation_id).await?;
    if pending.is_empty() {
        return Ok(None);
    }

    let arches: std::collections::HashSet<String> = worker_caps
        .iter()
        .flat_map(|(a, _)| a.iter().cloned())
        .collect();
    let checker = BuildabilityChecker::load(state, &pending, arches, evaluation_id).await?;
    let target = if checker.any_buildable(&pending, worker_caps) {
        EvaluationStatus::Building
    } else {
        EvaluationStatus::Waiting
    };
    let reason = if matches!(target, EvaluationStatus::Waiting) {
        Some(checker.compute_waiting_reason(&pending, worker_caps))
    } else {
        None
    };

    Ok(Some((target, reason)))
}

/// Self-heal a graph-stuck evaluation: reconcile stale `closure_complete`
/// flags to a fixpoint and re-promote, then re-assess. Recovers to `Building`
/// when the heal frees a dispatchable anchor; otherwise reports `GraphStuck`
/// with the blocked count so the stall is legible while later passes retry.
async fn attempt_graph_unstick(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    worker_caps: &[(Vec<String>, Vec<String>)],
) -> Result<(EvaluationStatus, Option<WaitingReason>)> {
    info!(%evaluation_id, "graph stuck: pool can build every pending anchor but none is dispatchable; self-healing");

    // The canonical healing pipeline in Unstick scope: edges_complete
    // restore, terminal-failed thaw, cache-trust reconcile, flag fixpoints,
    // promotion (see `gradient_db::reconcile`).
    gradient_db::reconcile_build_graph(
        &state.db(),
        gradient_db::ReconcileScope::Unstick(evaluation_id),
    )
    .await;

    if let Some((EvaluationStatus::Building, reason)) =
        assess_buildability(state, evaluation_id, worker_caps).await?
    {
        return Ok((EvaluationStatus::Building, reason));
    }

    let blocked = eval_pending_anchors(state, evaluation_id).await?.len() as u32;

    Ok((
        EvaluationStatus::Waiting,
        Some(WaitingReason::graph_stuck(blocked)),
    ))
}

/// Re-evaluate evaluations wedged in `graph_stuck` on a `.drv` our cache lost.
/// A build target's own `.drv` has no producer - only evaluation emits a `.drv`,
/// and the daemon-free server cannot reproduce one - so an anchor whose `.drv`
/// NAR is absent (never uploaded, or GC-reclaimed) can never dispatch and no
/// reconcile heals it. The sole recovery is a fresh evaluation of the same
/// commit ([`gradient_ci::trigger_drv_recovery`]), which re-materialises and
/// re-uploads the `.drv`. Runs after [`reconcile_waiting_state`] has persisted
/// the `graph_stuck` reason; the abort inside the trigger drops the stuck eval
/// out of `Waiting`, so each is handled once.
///
/// One-shot: a run already marked [`EvaluationKind::DrvRecovery`] that stalls
/// the same way is failed, not retried - a `.drv` a cold re-eval still cannot
/// persist is a real defect, not a transient upload miss, and re-triggering
/// would loop.
pub async fn recover_drv_stuck_evals(state: &Arc<ServerState>) -> Result<()> {
    let waiting = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(&state.worker_db)
        .await
        .context("fetch waiting evaluations")?;

    let now = gradient_types::now();
    for eval in waiting {
        let graph_stuck = eval
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json)
            .is_some_and(|r| matches!(r, WaitingReason::GraphStuck { .. }));
        if !graph_stuck || (now - eval.updated_at).num_seconds() < DRV_RECOVERY_GRACE_SECS {
            continue;
        }
        if !eval_blocked_on_unproducible_drv(state, eval.id).await? {
            continue;
        }

        if eval.kind == EvaluationKind::DrvRecovery {
            warn!(evaluation_id = %eval.id, "drv-recovery re-eval still blocked on its own missing .drv; failing (unrecoverable)");
            update_evaluation_status(&state.db(), eval, EvaluationStatus::Failed).await;
            continue;
        }

        let Some(task_id) = eval.task else {
            continue;
        };
        let task = match ETask::find_by_id(task_id).one(&state.worker_db).await {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                warn!(evaluation_id = %eval.id, error = %e, "drv recovery: task lookup failed");
                continue;
            }
        };

        match gradient_ci::trigger_drv_recovery(state.worker_db.inner(), &task, &eval).await {
            Ok(new_eval) => {
                info!(stuck = %eval.id, recovery = %new_eval.id, "auto-triggered .drv-recovery re-evaluation");
                gradient_ci::actions::dispatch_evaluation_created(&state.ci(), &new_eval).await;
            }
            Err(e) => {
                warn!(evaluation_id = %eval.id, error = %e, "failed to trigger .drv recovery")
            }
        }
    }

    Ok(())
}

/// True when a pending anchor of `evaluation_id` is dispatch-blocked solely by
/// its own missing `.drv`: its build dependencies are all satisfied and it is
/// not substitutable, yet neither the build-graph `drv_closure_cached` flag nor
/// the `.drv`'s own NAR-closure holds. That is the zone-B signature - a `.drv`
/// our cache never received or lost - distinct from a failed-dependency block,
/// whose anchors the dependency-failed cascade has already demoted.
async fn eval_blocked_on_unproducible_drv(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<bool> {
    let row = state
        .worker_db
        .inner()
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            unproducible_drv_block_sql(),
            [Value::Uuid(Some(evaluation_id.into_inner()))],
        ))
        .await
        .context("detect unproducible-drv block")?;

    Ok(row
        .and_then(|r| r.try_get::<bool>("", "blocked").ok())
        .unwrap_or(false))
}

fn unproducible_drv_block_sql() -> String {
    let deps_ready = gradient_db::graph_sql::deps_ready_predicate("db");
    let drv_nar_closure = gradient_db::graph_sql::drv_nar_closure_complete_predicate("db");
    let drv_nar_absent = gradient_db::graph_sql::drv_nar_absent_predicate("db");
    format!(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM build_job bj
            JOIN derivation_build db ON db.id = bj.derivation_build
            WHERE bj.evaluation = $1
              AND db.status IN ({created}, {queued})
              AND db.edges_complete
              AND NOT db.substitutable
              AND NOT db.drv_closure_cached
              AND NOT {drv_nar_closure}
              AND {drv_nar_absent}
              AND ({deps_ready})
        ) AS blocked
        "#,
        created = BuildStatus::Created as i32,
        queued = BuildStatus::Queued as i32,
    )
}

/// The non-terminal `derivation_build` anchors an evaluation still needs:
/// the anchors of its `build_job`s in Created/Queued/Building/FailedTransient.
async fn eval_pending_anchors(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<Vec<MDerivationBuild>> {
    use sea_orm::QuerySelect;
    let db = &state.worker_db;
    let anchor_ids: Vec<DerivationBuildId> = EBuildJob::find()
        .select_only()
        .column(CBuildJob::DerivationBuild)
        .filter(CBuildJob::Evaluation.eq(evaluation_id))
        .into_tuple::<DerivationBuildId>()
        .all(db)
        .await
        .context("fetch eval build_job anchors")?;
    if anchor_ids.is_empty() {
        return Ok(vec![]);
    }

    gradient_db::fetch_in_chunks(&anchor_ids, |chunk| async move {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Id.is_in(chunk))
            .filter(CDerivationBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
                BuildStatus::FailedTransient,
            ]))
            .all(db)
            .await
    })
    .await
    .context("fetch pending anchors")
}

/// The capability a pre-build evaluation needs to make progress: `Fetching`
/// wants a fetch-capable worker, every other pre-build state wants an
/// eval-capable one.
fn pre_build_capability(status: EvaluationStatus) -> Option<EvalCapability> {
    match status {
        EvaluationStatus::Fetching => Some(EvalCapability::Fetch),
        EvaluationStatus::Queued
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation => Some(EvalCapability::Eval),
        _ => None,
    }
}

/// Whether the connected pool provides `capability`.
fn capability_available(
    capability: EvalCapability,
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
) -> bool {
    match capability {
        EvalCapability::Fetch => fetch_capable_workers > 0,
        EvalCapability::Eval => eval_capable_workers > 0,
    }
}

/// Decide whether an *active* (non-`Waiting`) pre-build evaluation must stall.
///
/// Returns `Some((Waiting, reason))` when no worker provides the capability the
/// state needs - a `Fetching` eval needs `fetch`, every other pre-build state
/// needs `eval`. Returns `None` while the eval can progress. `Waiting` evals are
/// handled by [`decide_eval_recovery`], so this returns `None` for them.
fn decide_pre_build_target(
    current: EvaluationStatus,
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
    connected_workers: u32,
) -> Option<(EvaluationStatus, Option<WaitingReason>)> {
    let capability = pre_build_capability(current)?;
    if capability_available(capability, eval_capable_workers, fetch_capable_workers) {
        return None;
    }

    Some((
        EvaluationStatus::Waiting,
        Some(WaitingReason::eval_workers(capability, connected_workers)),
    ))
}

/// Recovery for a `Waiting` eval parked in a pre-build phase: unpark to `Queued`
/// once `capability` is back, otherwise refresh the reason with the live count.
fn decide_eval_recovery(
    capability: EvalCapability,
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
    connected_workers: u32,
) -> (EvaluationStatus, Option<WaitingReason>) {
    if capability_available(capability, eval_capable_workers, fetch_capable_workers) {
        (EvaluationStatus::Queued, None)
    } else {
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(capability, connected_workers)),
        )
    }
}

/// Update `evaluation.waiting_reason` only when the value actually changes.
///
/// Avoids a row-level UPDATE every reconcile cycle when the unmet capabilities
/// haven't shifted, which keeps `updated_at` from churning on the row.
pub(crate) async fn persist_waiting_reason(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    current: &Option<serde_json::Value>,
    new_reason: Option<&WaitingReason>,
) {
    let new_value = new_reason.map(|r| r.to_json());

    let unchanged = match (current, &new_value) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if unchanged {
        return;
    }

    let res = EEvaluation::update_many()
        .col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(new_value),
        )
        .filter(CEvaluation::Id.eq(evaluation_id))
        .exec(&state.worker_db)
        .await;

    if let Err(e) = res {
        warn!(error = %e, %evaluation_id, "failed to persist waiting_reason");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The zone-B detection must fire only on an anchor blocked *solely* by its
    /// own unimportable `.drv`: pending, edges complete, deps satisfied, not
    /// substitutable, and neither `.drv` signal true. Mis-shaping it would either
    /// re-eval healthy evals or miss the lost-`.drv` stall (no live DB in unit
    /// tests, so pin the SQL shape).
    #[test]
    fn unproducible_drv_block_sql_shape() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(unproducible_drv_block_sql());
        assert!(
            sql.contains(&format!(
                "db.status IN ({}, {})",
                BuildStatus::Created as i32,
                BuildStatus::Queued as i32
            )),
            "only pending anchors: {sql}"
        );
        for frag in [
            "db.edges_complete",
            "NOT db.substitutable",
            "NOT db.drv_closure_cached",
        ] {
            assert!(sql.contains(frag), "missing `{frag}`: {sql}");
        }
        assert!(
            sql.contains(&norm(
                gradient_db::graph_sql::drv_nar_closure_complete_predicate("db")
            )),
            "must require the .drv's own NAR closure to be absent: {sql}"
        );
        assert!(
            sql.contains("cached_path cp") && sql.contains("derivation_input_source"),
            "must embed the deps-ready predicate (all build deps and sources cached): {sql}"
        );
    }

    /// Only a genuinely absent `.drv` NAR justifies condemning an evaluation.
    ///
    /// The closure flags are also false while a present `.drv` simply has not
    /// converged, and re-evaluating cannot fix a flag - so keying on them alone
    /// burned an eval per stall and then failed it one-shot as "unrecoverable"
    /// with the `.drv` sitting in the cache the whole time. The block now
    /// additionally requires that no backed `cached_path` row exists for the
    /// `.drv`, which is the state a fresh evaluation actually repairs.
    #[test]
    fn unproducible_drv_block_requires_the_drv_nar_to_be_absent() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(unproducible_drv_block_sql());
        assert!(
            sql.contains(&norm(gradient_db::graph_sql::drv_nar_absent_predicate(
                "db"
            ))),
            "must require the .drv's own NAR to be missing, not merely unconverged: {sql}"
        );
    }

    fn eval_workers_view(r: &WaitingReason) -> (EvalCapability, u32) {
        match r {
            WaitingReason::EvalWorkers {
                capability,
                connected_workers,
            } => (*capability, *connected_workers),
            other => panic!("expected EvalWorkers variant, got {other:?}"),
        }
    }

    /// Regression for issue #268/#381: a Queued evaluation whose connected
    /// workers lack the `eval` capability stalls to Waiting with an `eval`
    /// `EvalWorkers` reason, reporting the total connected pool size.
    #[test]
    fn pre_build_target_queued_no_eval_worker_stalls_to_eval_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Queued, 0, 1, 3)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("stall carries a reason"));
        assert_eq!(cap, EvalCapability::Eval);
        assert_eq!(connected, 3);
    }

    /// #381: a `Fetching` eval needs a fetch-capable worker - an eval-only pool
    /// still strands it, with a `fetch` reason.
    #[test]
    fn pre_build_target_fetching_no_fetch_worker_stalls_to_fetch_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Fetching, 2, 0, 2)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("stall carries a reason"));
        assert_eq!(cap, EvalCapability::Fetch);
        assert_eq!(connected, 2);
    }

    #[test]
    fn pre_build_target_active_pre_build_with_capability_left_alone() {
        // Fetching needs fetch; Queued/Evaluating* need eval. With both present
        // the eval is progressing and must not be reconciled.
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Queued,
        ] {
            assert!(
                decide_pre_build_target(status, 1, 1, 1).is_none(),
                "{status:?} with capable workers must not be reconciled"
            );
        }
    }

    #[test]
    fn pre_build_target_ignores_waiting() {
        // Waiting recovery is decide_eval_recovery's job, not this function's.
        assert!(decide_pre_build_target(EvaluationStatus::Waiting, 0, 0, 0).is_none());
        assert!(decide_pre_build_target(EvaluationStatus::Waiting, 2, 2, 2).is_none());
    }

    #[test]
    fn eval_recovery_unparks_to_queued_when_capability_returns() {
        let (target, reason) = decide_eval_recovery(EvalCapability::Eval, 1, 0, 1);
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());

        let (target, reason) = decide_eval_recovery(EvalCapability::Fetch, 0, 1, 1);
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());
    }

    #[test]
    fn eval_recovery_refreshes_reason_while_capability_absent() {
        let (target, reason) = decide_eval_recovery(EvalCapability::Fetch, 5, 0, 5);
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("refresh carries a reason"));
        assert_eq!(cap, EvalCapability::Fetch);
        assert_eq!(connected, 5);
    }
}
