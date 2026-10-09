/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::DbContext;
use crate::graph::predicates::BUILDER_STATUSES;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::DbErr;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionChange {
    pub derivation: DerivationId,
    pub from: BuildStatus,
    pub to: BuildStatus,
}

impl TransitionChange {
    pub fn unchanged(derivation: DerivationId, status: BuildStatus) -> Self {
        Self {
            derivation,
            from: status,
            to: status,
        }
    }
}

/// Only the net move committed, and only the net move may fan out.
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

fn ci_reports(status: BuildStatus) -> bool {
    matches!(status, BuildStatus::Queued | BuildStatus::Building)
        || crate::state_machine::BuildStateMachine::is_terminal(&status)
}

/// The second round cannot need a third.
/// It is only moving rows between `Created` and `Queued`, both inside [`BUILDER_STATUSES`].
#[tracing::instrument(level = "debug", skip_all)]
pub async fn emit_transition_effects(
    ctx: &DbContext,
    changes: &[TransitionChange],
) -> Result<(), DbErr> {
    if changes.is_empty() {
        return Ok(());
    }

    ctx.startable_set.record(changes);
    announce(ctx, changes).await?;
    let Moved {
        regated,
        unwanted,
        gained,
    } = move_need(ctx, changes).await?;
    ctx.probe_requests.send(gained);
    if !regated.is_empty() {
        ctx.startable_set.record(&regated);
        announce(ctx, &regated).await?;
    }
    if !unwanted.is_empty() {
        super::eval_finalize::finalize_evals_for_derivations(ctx, &unwanted).await?;
    }

    Ok(())
}

fn is_builder(status: BuildStatus) -> bool {
    BUILDER_STATUSES.contains(&status)
}

fn need_moves(changes: &[TransitionChange]) -> Vec<DerivationId> {
    changes
        .iter()
        .filter(|c| is_builder(c.from) != is_builder(c.to))
        .map(|c| c.derivation)
        .collect()
}

async fn move_need(ctx: &DbContext, changes: &[TransitionChange]) -> Result<Moved, DbErr> {
    let db = &ctx.worker_db;
    let mut moved_out = Moved::default();
    for chunk in need_moves(changes).chunks(crate::IN_CHUNK_SIZE) {
        let moved = crate::graph::can_start::update_need(db, chunk).await?;
        moved_out
            .regated
            .extend(crate::graph::can_start::settle_need(db, &moved).await?);
        moved_out.gained.extend(moved.gained);
        moved_out.unwanted.extend(moved.lost);
    }

    Ok(moved_out)
}

#[derive(Debug, Default)]
struct Moved {
    regated: Vec<TransitionChange>,
    unwanted: Vec<DerivationId>,
    gained: Vec<DerivationId>,
}

async fn announce(ctx: &DbContext, changes: &[TransitionChange]) -> Result<(), DbErr> {
    if changes.is_empty() {
        return Ok(());
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
        .await?
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
        .await?
        .into_iter()
        .map(|ep| (ep.evaluation, ep.derivation))
        .collect();

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

    crate::task_board::dep_counts::bump_graph_version(db, &moved).await?;

    for c in changes {
        let Some(jobs) = jobs_by_drv.get(&c.derivation) else {
            continue;
        };
        for job in jobs {
            ctx.events
                .publish(gradient_types::events::build::StatusChanged {
                    build_id: job.id,
                    derivation_build: job.derivation_build,
                    evaluation_id: job.evaluation,
                    status: i32::from(c.to) as i16,
                });

            if ci_reports(c.to) && entry_keys.contains(&(job.evaluation, job.derivation)) {
                crate::deliveries::events::record(
                    db,
                    &ctx.events,
                    gradient_types::events::build::Reported {
                        build_id: job.id,
                        derivation_build: job.derivation_build,
                        evaluation_id: job.evaluation,
                        derivation: job.derivation,
                        status: i32::from(c.to) as i16,
                        ..Default::default()
                    },
                )
                .await?;
            }
        }
    }

    if changes.iter().any(|c| {
        matches!(c.to, BuildStatus::Completed | BuildStatus::Substituted) && c.from != c.to
    }) {
        ctx.events
            .publish(gradient_types::events::cache::Changed {});
    }

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
        super::eval_finalize::check_evaluation_done(ctx, evaluation_id).await?;
    }

    let finished: Vec<DerivationId> = changes
        .iter()
        .filter(|c| c.from != c.to && crate::state_machine::BuildStateMachine::is_terminal(&c.to))
        .map(|c| c.derivation)
        .collect();
    if !finished.is_empty() {
        let attempts =
            crate::scheduling::build_attempt::latest_attempts_by_derivation(db, &finished).await?;
        super::logging::enqueue_log_finalize(db, attempts.into_values()).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_reports_matches_the_git_host_check_lifecycle() {
        assert!(ci_reports(BuildStatus::Queued));
        assert!(ci_reports(BuildStatus::Building));
        assert!(ci_reports(BuildStatus::Completed));
        assert!(ci_reports(BuildStatus::DependencyFailed));
        assert!(ci_reports(BuildStatus::Aborted));
        assert!(!ci_reports(BuildStatus::Created));
        assert!(!ci_reports(BuildStatus::FailedTransient));
    }

    #[test]
    fn need_moves_are_every_crossing_of_the_builder_boundary() {
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
            need_moves(&[
                change(thawed, BuildStatus::FailedPermanent, BuildStatus::Created),
                change(finished, BuildStatus::Building, BuildStatus::Completed),
                change(aborted, BuildStatus::Queued, BuildStatus::Aborted),
                change(promoted, BuildStatus::Created, BuildStatus::Queued),
            ]),
            vec![thawed, finished, aborted],
        );
    }

    #[test]
    fn re_gating_can_never_need_a_third_round() {
        let d = DerivationId::now_v7();
        for (from, to) in [
            (BuildStatus::Created, BuildStatus::Queued),
            (BuildStatus::Queued, BuildStatus::Created),
        ] {
            let moved = need_moves(&[TransitionChange {
                derivation: d,
                from,
                to,
            }]);
            assert!(moved.is_empty(), "{from:?} to {to:?}");
        }
    }

    #[test]
    fn an_unchanged_announcement_moves_no_need() {
        assert!(
            need_moves(&[TransitionChange::unchanged(
                DerivationId::now_v7(),
                BuildStatus::Completed,
            )])
            .is_empty()
        );
    }

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

    #[tokio::test]
    async fn a_finished_entry_point_owes_a_report_and_its_log() {
        let d = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let job = MBuildJob {
            id: BuildJobId::now_v7(),
            evaluation,
            derivation: d,
            ..Default::default()
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![job.clone()]])
            .append_query_results([vec![MEntryPoint {
                evaluation,
                derivation: d,
                ..Default::default()
            }]])
            .append_query_results(vec![
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new();
                3
            ])
            .append_exec_results(vec![sea_orm::MockExecResult::default(); 4])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        emit_transition_effects(
            &ctx,
            &[TransitionChange {
                derivation: d,
                from: BuildStatus::Building,
                to: BuildStatus::Completed,
            }],
        )
        .await
        .expect("effects");
        crate::test_ctx::settle(ctx).await;

        let log = crate::pool::statements(pool.into_transaction_log());
        let reports: Vec<&String> = log
            .iter()
            .filter(|s| s.contains("INSERT INTO pending_delivery"))
            .collect();
        assert_eq!(
            reports.len(),
            1,
            "one report for the one entry point: {log:?}"
        );
        assert!(reports[0].contains(&job.id.to_string()), "{reports:?}");
        assert!(
            log.iter()
                .any(|s| s.contains("JOIN derivation_build b ON b.id = a.derivation_build")),
            "the finished build's log is asked for: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_re_announce_never_asks_to_finalize_a_log_again() {
        let d = DerivationId::now_v7();
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<MEntryPoint>::new()])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        emit_transition_effects(
            &ctx,
            &[TransitionChange::unchanged(d, BuildStatus::Completed)],
        )
        .await
        .expect("effects");
        crate::test_ctx::settle(ctx).await;

        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            !log.iter()
                .any(|s| s.contains("JOIN derivation_build b ON b.id = a.derivation_build")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn what_gains_need_is_handed_to_the_upstream_probe() {
        let crossed = DerivationId::now_v7();
        let gained = DerivationId::now_v7();
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
            .append_query_results(std::iter::repeat_n(
                vec![std::collections::BTreeMap::from([
                    (
                        "derivation".to_owned(),
                        sea_orm::Value::from(gained.into_inner()),
                    ),
                    ("wanted".to_owned(), sea_orm::Value::from(true)),
                ])],
                2,
            ))
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let (ctx, _pool, mut probes) = crate::test_ctx::ctx_with_probes(db).await;

        let moved = move_need(
            &ctx,
            &[TransitionChange {
                derivation: crossed,
                from: BuildStatus::Created,
                to: BuildStatus::Completed,
            }],
        )
        .await
        .expect("move need");
        ctx.probe_requests.send(moved.gained);
        crate::test_ctx::settle(ctx).await;

        assert_eq!(
            probes.try_recv().expect("the gained set reaches the probe"),
            vec![gained]
        );
    }

    #[tokio::test]
    async fn a_boundary_crossing_updates_the_closure_and_settles_the_queue() {
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
            .append_query_results(std::iter::repeat_n(
                vec![std::collections::BTreeMap::from([
                    (
                        "derivation".to_owned(),
                        sea_orm::Value::from(lost.into_inner()),
                    ),
                    ("wanted".to_owned(), sea_orm::Value::from(false)),
                ])],
                2,
            ))
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        let moved = move_need(
            &ctx,
            &[TransitionChange {
                derivation: crossed,
                from: BuildStatus::Created,
                to: BuildStatus::Completed,
            }],
        )
        .await
        .expect("move need");
        drop(ctx);

        assert_eq!(
            moved.unwanted,
            vec![lost],
            "what lost its need is reported, so the evaluations waiting on it can settle"
        );

        let log = crate::pool::statements(pool.into_transaction_log()).join(" ");
        assert!(log.contains("SET LOCAL work_mem"), "{log}");
        assert!(log.contains("SET wanted ="), "{log}");
        assert!(
            !log.contains("SELECT DISTINCT e.dependency FROM derivation_dependency"),
            "the one-hop re-gate must be gone: {log}"
        );
    }
}
