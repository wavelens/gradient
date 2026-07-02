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
//! consequences (dep-count deltas, board events, CI checks) firing - the root
//! cause of the historical dead-zone class.

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
    /// CI state without shifting dep-count histograms. Used when only the
    /// derivation set is known, not the transition that produced it.
    pub fn unchanged(derivation: DerivationId, status: BuildStatus) -> Self {
        Self { derivation, from: status, to: status }
    }
}

/// Statuses the CI side reports on: `Queued` (pending), `Building` (running),
/// and every terminal state. `Created`/`FailedTransient` are internal.
fn ci_reports(status: BuildStatus) -> bool {
    matches!(status, BuildStatus::Queued | BuildStatus::Building)
        || crate::state_machine::BuildStateMachine::is_terminal(&status)
}

/// Fan out the consequences of `changes`: per-entry-point dep-count deltas,
/// board `BuildStatusChanged` events for every referencing `build_job`, one
/// `CacheChanged` on any terminal success, and the CI status reactor for entry
/// points. Reactor calls are spawned (they talk to external forges); everything
/// else is awaited so a failure is visible at the call site's log context.
pub async fn emit_transition_effects(ctx: &DbContext, changes: &[TransitionChange]) {
    if changes.is_empty() {
        return;
    }

    let db = &ctx.worker_db;

    for c in changes {
        if c.from == c.to {
            continue;
        }
        if let Err(e) =
            crate::dep_closure::apply_dep_count_delta(db, c.derivation, c.from, c.to).await
        {
            error!(error = %e, derivation = %c.derivation, "failed to update entry-point dep counts");
        }
    }

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
                let action_ctx = ctx.clone();
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

    if changes
        .iter()
        .any(|c| matches!(c.to, BuildStatus::Completed | BuildStatus::Substituted) && c.from != c.to)
    {
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
}
