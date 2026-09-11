/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one place a build-graph transition's consequences fan out. Both mutation
//! models feed it: the single-row state-machine path
//! ([`super::update_derivation_build_status`]) and the bulk SQL sweeps
//! (promotion, cascades, reconciles, abort), which return the
//! [`TransitionChange`]s they made. Routing every mover through one emitter is
//! what makes it structurally impossible to move an anchor without its
//! consequences (the evaluation graph version, board events, CI checks) firing -
//! the root cause of the historical dead-zone class.

use crate::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use std::collections::{HashMap, HashSet};
use tracing::error;

/// One anchor status move, as reported by the path that made it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionChange {
    pub derivation: DerivationId,
    pub from: BuildStatus,
    pub to: BuildStatus,
}

impl TransitionChange {
    /// A "re-announce current status" change (`from == to`): fans out board and
    /// CI state without invalidating the histogram cache. Used when only the
    /// derivation set is known, not the transition that produced it.
    pub fn unchanged(derivation: DerivationId, status: BuildStatus) -> Self {
        Self {
            derivation,
            from: status,
            to: status,
        }
    }
}

/// One entry per derivation, first `from` to last `to`, in first-seen order, with
/// the derivations that ended where they started dropped entirely.
///
/// For a caller that moves the same anchor twice inside ONE transaction: only the
/// net move committed, so only the net move may fan out. Emitting the steps instead
/// would announce a status to the board and the CI reactor that no reader can ever
/// observe, and would invalidate the histogram cache for a move no reader can see.
pub fn collapse_transitions(changes: Vec<TransitionChange>) -> Vec<TransitionChange> {
    let mut order: Vec<DerivationId> = Vec::new();
    let mut net: HashMap<DerivationId, TransitionChange> = HashMap::new();
    for change in changes {
        match net.entry(change.derivation) {
            std::collections::hash_map::Entry::Occupied(mut e) => e.get_mut().to = change.to,
            std::collections::hash_map::Entry::Vacant(e) => {
                order.push(change.derivation);
                e.insert(change);
            }
        }
    }

    order
        .into_iter()
        .filter_map(|d| net.remove(&d))
        .filter(|c| c.from != c.to)
        .collect()
}

/// Statuses the CI side reports on: `Queued` (pending), `Building` (running),
/// and every terminal state. `Created`/`FailedTransient` are internal.
fn ci_reports(status: BuildStatus) -> bool {
    matches!(status, BuildStatus::Queued | BuildStatus::Building)
        || crate::state_machine::BuildStateMachine::is_terminal(&status)
}

/// Fan out the consequences of `changes`: the evaluation graph version that
/// invalidates the per-entry-point histogram cache,
/// board `BuildStatusChanged` events for every referencing `build_job`, one
/// `CacheChanged` on any terminal success, and the CI status reactor for entry
/// points. Reactor calls are spawned (they talk to external forges); everything
/// else is awaited so a failure is visible at the call site's log context.
pub async fn emit_transition_effects(ctx: &DbContext, changes: &[TransitionChange]) {
    if changes.is_empty() {
        return;
    }

    let db = &ctx.worker_db;

    let derivations: Vec<DerivationId> = changes.iter().map(|c| c.derivation).collect();
    let jobs_by_drv: HashMap<DerivationId, Vec<MBuildJob>> =
        crate::fetch_in_chunks(&derivations, |chunk| async move {
            use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
            EBuildJob::find()
                .filter(CBuildJob::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .fold(HashMap::new(), |mut m, j| {
            m.entry(j.derivation).or_default().push(j);
            m
        });

    let entry_keys: HashSet<(EvaluationId, DerivationId)> =
        crate::fetch_in_chunks(&derivations, |chunk| async move {
            use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
            EEntryPoint::find()
                .filter(CEntryPoint::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|ep| (ep.evaluation, ep.derivation))
        .collect();

    // One bump per emit covers every evaluation a moved anchor belongs to; their
    // cached histograms recompute on the next read. A failed bump is logged rather
    // than propagated, because the board events and CI checks below must fan out
    // regardless; `dep_counts::DEP_COUNTS_MAX_AGE_SECS` is what heals a lost one.
    let moved: Vec<EvaluationId> = changes
        .iter()
        .filter(|c| c.from != c.to)
        .flat_map(|c| {
            jobs_by_drv
                .get(&c.derivation)
                .into_iter()
                .flatten()
                .map(|j| j.evaluation)
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if let Err(e) = crate::dep_counts::bump_graph_version(db, &moved).await {
        error!(
            error = %e,
            evaluations = moved.len(),
            max_age_secs = crate::dep_counts::DEP_COUNTS_MAX_AGE_SECS,
            "failed to bump the graph version; the histograms heal on the age ceiling"
        );
    }

    for c in changes {
        let Some(jobs) = jobs_by_drv.get(&c.derivation) else {
            continue;
        };
        for job in jobs {
            let _ = ctx
                .board_events
                .send(gradient_types::BoardEvent::BuildStatusChanged {
                    evaluation_id: job.evaluation.into_inner(),
                    build_id: job.id.into_inner(),
                    status: i32::from(c.to) as i16,
                });

            // Only declared entry points get a forge check; skip the spawn for
            // intermediate builds instead of no-opping inside the reactor.
            if ci_reports(c.to) && entry_keys.contains(&(job.evaluation, job.derivation)) {
                let action_ctx = ctx.detached();
                let job = job.clone();
                let to = c.to;
                ctx.shutdown.spawn(async move {
                    action_ctx
                        .reactor
                        .on_build_status_changed(&action_ctx, job, to)
                        .await;
                });
            }
        }
    }

    if changes.iter().any(|c| {
        matches!(c.to, BuildStatus::Completed | BuildStatus::Substituted) && c.from != c.to
    }) {
        let _ = ctx
            .board_events
            .send(gradient_types::BoardEvent::CacheChanged);
    }

    // A terminal transition may have settled its referencing evaluations; the
    // finalize decision is graph-derived and idempotent, so checking here (for
    // every mover, bulk or single-row) closes the "eval hangs Building because
    // a bulk sweep bypassed the reactive finalize hook" dead-zone class.
    let terminal_evals: HashSet<EvaluationId> = changes
        .iter()
        .filter(|c| crate::state_machine::BuildStateMachine::is_terminal(&c.to))
        .flat_map(|c| {
            jobs_by_drv
                .get(&c.derivation)
                .into_iter()
                .flatten()
                .map(|j| j.evaluation)
        })
        .collect();
    for evaluation_id in terminal_evals {
        if let Err(e) = super::eval_finalize::check_evaluation_done(ctx, evaluation_id).await {
            error!(error = %e, %evaluation_id, "eval finalize after transition failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI checks track Queued (pending), Building (running), and terminals;
    /// internal states (Created, FailedTransient) must not post to forges.
    #[test]
    fn ci_reports_matches_the_forge_check_lifecycle() {
        assert!(ci_reports(BuildStatus::Queued));
        assert!(ci_reports(BuildStatus::Building));
        assert!(ci_reports(BuildStatus::Completed));
        assert!(ci_reports(BuildStatus::DependencyFailed));
        assert!(ci_reports(BuildStatus::Aborted));
        assert!(!ci_reports(BuildStatus::Created));
        assert!(!ci_reports(BuildStatus::FailedTransient));
    }

    #[test]
    fn unchanged_marks_from_equal_to() {
        let d = DerivationId::now_v7();
        let c = TransitionChange::unchanged(d, BuildStatus::Completed);
        assert_eq!(c.from, c.to);
        assert_eq!(c.derivation, d);
    }

    /// An anchor promoted and then pulled back inside one transaction committed
    /// nothing, so it must fan out nothing: emitting the two steps announces a
    /// `Queued` no reader can observe and bumps the graph version for it. An
    /// anchor that genuinely moved keeps its move, and the order of first sight is
    /// preserved.
    #[test]
    fn a_move_and_its_undo_collapse_away_while_a_real_move_survives() {
        let bounced = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let change = |derivation, from, to| TransitionChange {
            derivation,
            from,
            to,
        };

        let net = collapse_transitions(vec![
            change(bounced, BuildStatus::Created, BuildStatus::Queued),
            change(promoted, BuildStatus::Created, BuildStatus::Queued),
            change(bounced, BuildStatus::Queued, BuildStatus::Created),
        ]);

        assert_eq!(net.len(), 1, "only the net move survives: {net:?}");
        assert_eq!(net[0].derivation, promoted);
        assert_eq!(
            (net[0].from, net[0].to),
            (BuildStatus::Created, BuildStatus::Queued)
        );
    }

    /// A chain that ends somewhere else collapses to its endpoints, not to its
    /// last step: the board and the CI reactor see the endpoints, not the steps.
    #[test]
    fn a_chain_collapses_to_its_endpoints() {
        let d = DerivationId::now_v7();
        let net = collapse_transitions(vec![
            TransitionChange {
                derivation: d,
                from: BuildStatus::Created,
                to: BuildStatus::Queued,
            },
            TransitionChange {
                derivation: d,
                from: BuildStatus::Queued,
                to: BuildStatus::Building,
            },
        ]);

        assert_eq!(net.len(), 1);
        assert_eq!(
            (net[0].from, net[0].to),
            (BuildStatus::Created, BuildStatus::Building)
        );
    }
}
