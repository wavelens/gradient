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
//! consequences (the evaluation graph version, board events, CI checks, the
//! demand its direct inputs gain or lose) firing - the root cause of the
//! historical dead-zone class.

use crate::DbContext;
use crate::graph_sql::BUILDER_STATUSES;
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

/// Fan out the consequences of `changes`: everything [`announce`] does, then the
/// demand its direct inputs gained or lost, whose own moves are announced in turn.
///
/// That second round cannot need a third. It only ever moves rows between
/// `Created` and `Queued`, both of which are in [`BUILDER_STATUSES`], so no row it
/// touches crosses the boundary [`demand_moves`] keys on.
///
/// Losing demand settles work without moving a status, and `announce`'s finalize
/// runs before the recompute that takes it away, so the evaluations naming what
/// lost it are asked again here. Nothing else would ask: no anchor of theirs need
/// have transitioned at all (#666).
pub async fn emit_transition_effects(ctx: &DbContext, changes: &[TransitionChange]) {
    if changes.is_empty() {
        return;
    }

    announce(ctx, changes).await;
    let (regated, undemanded) = move_demand(ctx, changes).await;
    if !regated.is_empty() {
        announce(ctx, &regated).await;
    }
    if !undemanded.is_empty()
        && let Err(e) = super::eval_finalize::finalize_evals_for_derivations(ctx, &undemanded).await
    {
        error!(error = %e, "eval finalize after a demand loss failed");
    }
}

/// Whether anchor `{status}` is one an evaluation will still have built, and so
/// one that still needs its inputs in our cache.
fn is_builder(status: BuildStatus) -> bool {
    BUILDER_STATUSES.contains(&status)
}

/// Every anchor whose transition carried it across the builder statuses, in either
/// direction: into them its inputs are wanted again, out of them they are not, and
/// its own stored demand is stale either way.
///
/// A substitutable anchor is a relay rather than a builder and demands nothing, but
/// the flag is not on a [`TransitionChange`]; the recompute reads it, so naming one
/// is a wasted row and never a wrong move.
fn demand_moves(changes: &[TransitionChange]) -> Vec<DerivationId> {
    changes
        .iter()
        .filter(|c| is_builder(c.from) != is_builder(c.to))
        .map(|c| c.derivation)
        .collect()
}

/// Recompute demand below every anchor that just became, or stopped being, something
/// this fleet will build, and settle the queue against what moved. The two statements
/// embed [`crate::graph_sql::gates_predicate`], so the candidate list is a bound and
/// never a claim.
///
/// An anchor already `Building` keeps building: [`crate::readiness::unpromote_ungated`]
/// moves only `Queued` rows. The bytes a running build produces are cached and useful,
/// while an abort throws the work away and complicates attempt attribution.
async fn move_demand(
    ctx: &DbContext,
    changes: &[TransitionChange],
) -> (Vec<TransitionChange>, Vec<DerivationId>) {
    let db = &ctx.worker_db;
    let mut regated = Vec::new();
    let mut undemanded = Vec::new();
    for chunk in demand_moves(changes).chunks(crate::IN_CHUNK_SIZE) {
        let moved = match crate::readiness::recompute_demand(db, chunk).await {
            Ok(moved) => moved,
            Err(e) => {
                error!(error = %e, "failed to recompute what an anchor demands");
                continue;
            }
        };

        for gained in moved.gained.chunks(crate::IN_CHUNK_SIZE) {
            match crate::readiness::promote(db, gained).await {
                Ok(changes) => regated.extend(changes),
                Err(e) => error!(error = %e, "failed to queue what an anchor demands"),
            }
        }
        for lost in moved.lost.chunks(crate::IN_CHUNK_SIZE) {
            match crate::readiness::unpromote_ungated(db, lost).await {
                Ok(changes) => regated.extend(changes),
                Err(e) => error!(error = %e, "failed to release undemanded anchors"),
            }
        }
        undemanded.extend(moved.lost);
    }

    (regated, undemanded)
}

/// The graph version that invalidates the per-entry-point histogram cache, board
/// `BuildStatusChanged` events for every referencing `build_job`, one
/// `CacheChanged` on any terminal success, and the CI status reactor for entry
/// points. Reactor calls are spawned (they talk to external forges); everything
/// else is awaited so a failure is visible at the call site's log context.
async fn announce(ctx: &DbContext, changes: &[TransitionChange]) {
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

    /// Demand follows the builder boundary, not "terminal": an anchor thawed back
    /// into the queue makes its inputs wanted again, and one that leaves for ANY
    /// non-builder status (a success and an abort alike) stops wanting them. Which
    /// way it crossed does not matter here, because the recompute is absolute over
    /// the region either way and a thaw needs its own stale value rewritten just as
    /// much as a finish does.
    #[test]
    fn demand_moves_are_every_crossing_of_the_builder_boundary() {
        let thawed = DerivationId::now_v7();
        let finished = DerivationId::now_v7();
        let aborted = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let change = |derivation, from, to| TransitionChange {
            derivation,
            from,
            to,
        };

        assert_eq!(
            demand_moves(&[
                change(thawed, BuildStatus::FailedPermanent, BuildStatus::Created),
                change(finished, BuildStatus::Building, BuildStatus::Completed),
                change(aborted, BuildStatus::Queued, BuildStatus::Aborted),
                change(promoted, BuildStatus::Created, BuildStatus::Queued),
            ]),
            vec![thawed, finished, aborted],
        );
    }

    /// The second announce round is only safe because nothing it moves can cross
    /// the boundary again: promotion and un-promotion both stay inside the builder
    /// statuses, so one round of re-gating is the whole fixpoint.
    #[test]
    fn re_gating_can_never_demand_a_third_round() {
        let d = DerivationId::now_v7();
        for (from, to) in [
            (BuildStatus::Created, BuildStatus::Queued),
            (BuildStatus::Queued, BuildStatus::Created),
        ] {
            let moved = demand_moves(&[TransitionChange {
                derivation: d,
                from,
                to,
            }]);
            assert!(moved.is_empty(), "{from:?} to {to:?}");
        }
    }

    /// A re-announce carries no move, so it must re-gate nothing.
    #[test]
    fn an_unchanged_announcement_moves_no_demand() {
        assert!(
            demand_moves(&[TransitionChange::unchanged(
                DerivationId::now_v7(),
                BuildStatus::Completed,
            )])
            .is_empty()
        );
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

    /// An anchor crossing the boundary recomputes its whole pending closure, not one
    /// hop: the source FODs under a relayed anchor were built because a one-hop
    /// re-gate never reached them (#666).
    #[tokio::test]
    async fn a_boundary_crossing_recomputes_the_closure_and_settles_the_queue() {
        let crossed = DerivationId::now_v7();
        let lost = DerivationId::now_v7();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
                sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .append_query_results([vec![std::collections::BTreeMap::from([
                (
                    "derivation".to_owned(),
                    sea_orm::Value::from(lost.into_inner()),
                ),
                ("demanded".to_owned(), sea_orm::Value::from(false)),
            ])]])
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        let (_, undemanded) = move_demand(
            &ctx,
            &[TransitionChange {
                derivation: crossed,
                from: BuildStatus::Created,
                to: BuildStatus::Completed,
            }],
        )
        .await;
        drop(ctx);

        assert_eq!(
            undemanded,
            vec![lost],
            "what lost its demand is reported, so the evaluations waiting on it can settle"
        );

        let log = crate::pool::statements(pool.into_transaction_log()).join(" ");
        assert!(log.contains("SET LOCAL work_mem"), "{log}");
        assert!(log.contains("SET demanded ="), "{log}");
        assert!(
            !log.contains("SELECT DISTINCT e.dependency FROM derivation_dependency"),
            "the one-hop re-gate must be gone: {log}"
        );
    }
}
