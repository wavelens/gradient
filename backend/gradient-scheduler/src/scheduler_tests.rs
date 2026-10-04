/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use gradient_types::ids::*;

use gradient_wire::types::{
    BuildJob, BuildSpec, BuildSpecKind, CandidateScore, FlakeJob, FlakeStep, GradientCapabilities,
    JobKind,
};

use super::actor::WorkerCapabilities;
use super::jobs::{PendingBuildJob, PendingEvalJob};
use super::{ReportedTimeline, Scheduler};
use gradient_pool::session_port::{SessionPort, SessionSignal};
use tokio::sync::mpsc;

async fn test_scheduler() -> Arc<Scheduler> {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let ok = MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![ok; 32])
        .into_connection();
    test_scheduler_with(db).await
}

async fn test_scheduler_with(db: sea_orm::DatabaseConnection) -> Arc<Scheduler> {
    use gradient_test_support::prelude::*;

    let state = test_state(db);
    let scheduler = Arc::new(Scheduler::new(state));
    scheduler.spawn_core(None).await.expect("core actor");
    scheduler
}

pub(crate) fn port() -> (Arc<dyn SessionPort>, mpsc::UnboundedReceiver<SessionSignal>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (Arc::new(tx), rx)
}

async fn register(
    scheduler: &Scheduler,
    worker: &str,
    caps: GradientCapabilities,
    peers: HashSet<ProjectId>,
) -> mpsc::UnboundedReceiver<SessionSignal> {
    let (session, signals) = port();
    scheduler
        .register_worker(worker, caps, peers, session)
        .await
        .expect("register");
    signals
}

pub(crate) fn eval_job(peer: ProjectId) -> PendingEvalJob {
    PendingEvalJob {
        evaluation_id: EvaluationId::now_v7(),
        task_id: None,
        project_id: peer,
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            steps: vec![FlakeStep::EvaluateDerivations],
            source: gradient_wire::types::FlakeSource::Repository {
                url: "https://example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
            input_update: None,
        },
        required_paths: vec![],
        queued_at: gradient_types::now(),
        ready_at: gradient_types::now(),
        rescore_count: 0,
        prioritized: false,
        build_request: false,
        history: Default::default(),
        walk_mode: Default::default(),
    }
}

pub(crate) fn build_job(
    evaluation_id: EvaluationId,
    peer: ProjectId,
    derivation_build: DerivationBuildId,
) -> PendingBuildJob {
    PendingBuildJob {
        derivation_build,
        derivation: DerivationId::now_v7(),
        evaluation_id,
        project_id: peer,
        job: BuildJob {
            builds: vec![BuildSpec {
                build_id: derivation_build.to_string(),
                drv_path: "aaaa-hello.drv".into(),
                kind: BuildSpecKind::Build,
                is_fixed_output: false,
                outputs: vec![],
                timeout_secs: None,
                max_silent_secs: None,
            }],
            requirement: gradient_wire::types::BuildRequirement {
                architecture: "x86_64-linux".into(),
                required_features: vec![],
            },
        },
        required_paths: vec![],
        dependency_count: 0,
        closure_size: None,
        prefer_local_build: false,
        is_fixed_output: false,
        history: gradient_pool::score::HistoryPrediction::default(),
        queued_at: gradient_types::now(),
        ready_at: gradient_types::now(),
        rescore_count: 0,
        prioritized: false,
        build_request: false,
        pname: None,
        substitute: false,
    }
}

pub(crate) fn eval_worker_caps() -> GradientCapabilities {
    GradientCapabilities {
        eval: true,
        ..GradientCapabilities::default()
    }
}

fn build_worker_caps() -> GradientCapabilities {
    GradientCapabilities {
        build: true,
        ..GradientCapabilities::default()
    }
}

#[tokio::test]
async fn test_enqueue_and_get_candidates() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();

    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("j2".into(), eval_job(peer))
        .await
        .unwrap();

    let candidates = scheduler.get_job_candidates("w1").await;
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn enqueue_signals_offers_with_a_rising_generation() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let mut signals = register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("j2".into(), eval_job(peer))
        .await
        .unwrap();

    assert_eq!(signals.recv().await, Some(SessionSignal::Offers(1)));
    assert_eq!(signals.recv().await, Some(SessionSignal::Offers(2)));
    let offer = scheduler.get_new_job_candidates("w1").await;
    assert_eq!(offer.candidates.len(), 2);
    assert_eq!(
        offer.generation, 2,
        "the offer carries the generation it answers"
    );
    assert!(
        scheduler
            .get_new_job_candidates("w1")
            .await
            .candidates
            .is_empty(),
        "a second fetch is a delta and finds nothing new"
    );
}

#[tokio::test]
async fn a_reenqueued_eval_job_is_offered_again_in_the_delta() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    let job = eval_job(peer);
    let job_id = crate::jobs::eval_job_key(job.evaluation_id);
    scheduler
        .enqueue_eval_job(job_id.clone(), job.clone())
        .await
        .unwrap();
    assert_eq!(
        scheduler
            .get_new_job_candidates("w1")
            .await
            .candidates
            .len(),
        1
    );

    scheduler
        .enqueue_eval_job(job_id.clone(), job)
        .await
        .unwrap();
    let offer = scheduler.get_new_job_candidates("w1").await;
    assert_eq!(
        offer
            .candidates
            .iter()
            .map(|c| c.job_id.as_str())
            .collect::<Vec<_>>(),
        vec![job_id.as_str()]
    );
}

#[tokio::test]
async fn assigner_kick_is_retained_when_not_awaiting() {
    use std::sync::atomic::Ordering;
    let scheduler = test_scheduler().await;
    let before = scheduler.kick_gen.load(Ordering::Relaxed);
    scheduler.kick_assigner();
    assert_eq!(
        scheduler.kick_gen.load(Ordering::Relaxed),
        before + 1,
        "a kick must advance the generation the dispatcher compares against"
    );
}

/// A capability heartbeat must not refresh the waiting state inline. The refresh blocked the
/// per-connection read loop, and the worker's next `CacheQuery` timed out after 75 s.
#[tokio::test]
async fn capability_update_kicks_the_assigner_instead_of_repairing_inline() {
    let scheduler = test_scheduler().await;
    scheduler
        .update_worker_capabilities(
            "peer-x",
            WorkerCapabilities {
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: 4,
                cpu_count: 8,
                ram_total_mb: 16_000,
                cpu_core_score: 100,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        scheduler
            .kick_gen
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "capability update must kick the dispatch loop, not refresh inline"
    );
}

#[tokio::test]
async fn test_candidates_filtered_by_authorized_peers() {
    let scheduler = test_scheduler().await;
    let peer_a = ProjectId::now_v7();
    let peer_b = ProjectId::now_v7();

    register(
        &scheduler,
        "w1",
        eval_worker_caps(),
        HashSet::from([peer_a]),
    )
    .await;

    scheduler
        .enqueue_eval_job("ja".into(), eval_job(peer_a))
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("jb".into(), eval_job(peer_b))
        .await
        .unwrap();

    let candidates = scheduler.get_job_candidates("w1").await;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].job_id, "ja");
}

#[tokio::test]
async fn test_score_assignment_flow() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();

    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();

    scheduler
        .record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        )
        .await;
    let assignment = scheduler.request_job("w1", JobKind::Flake).await;

    assert!(assignment.is_some());
    assert_eq!(assignment.unwrap().job_id(), "j1");
    assert_eq!(scheduler.pending_job_count().await, 0);
}

#[tokio::test]
async fn test_job_rejected_requeues() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();

    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();

    scheduler.request_job("w1", JobKind::Flake).await;
    assert_eq!(scheduler.pending_job_count().await, 0);

    scheduler.job_rejected("w1", "j1").await;
    assert_eq!(scheduler.pending_job_count().await, 1);
}

#[tokio::test]
async fn a_rejected_build_closes_the_attempt_its_assignment_opened() {
    use crate::jobs::{PendingJob, build_job_key};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let ok = MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![ok; 32])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let shared = DerivationBuildId::now_v7();
    let (session, _signals) = port();
    scheduler
        .reattach_worker(
            "w1",
            build_worker_caps(),
            HashSet::new(),
            session,
            vec![crate::jobs::Reattached::single(
                build_job_key(shared),
                PendingJob::Build(build_job(
                    EvaluationId::now_v7(),
                    ProjectId::now_v7(),
                    shared,
                )),
            )],
        )
        .await
        .expect("reattach");

    scheduler.job_rejected("w1", &build_job_key(shared)).await;

    let log = log_db.into_transaction_log();
    let close = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| {
            s.sql
                .starts_with("UPDATE \"build_attempt\" SET \"outcome\"")
        })
        .expect("the attempt of the rejected assignment is closed");
    assert!(
        format!("{:?}", close.values).contains(&shared.to_string()),
        "{:?}",
        close.values
    );
}

#[tokio::test]
async fn test_worker_disconnect_requeues_jobs() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();

    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("j2".into(), eval_job(peer))
        .await
        .unwrap();

    scheduler.request_job("w1", JobKind::Flake).await;
    scheduler.request_job("w1", JobKind::Flake).await;

    assert_eq!(scheduler.pending_job_count().await, 0);

    scheduler.unregister_worker("w1").await;
    assert_eq!(scheduler.pending_job_count().await, 2);
    assert_eq!(scheduler.worker_count().await, 0);

    register(&scheduler, "w2", eval_worker_caps(), HashSet::new()).await;
    let candidates = scheduler.get_job_candidates("w2").await;
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn test_update_authorized_peers_expands_access() {
    let scheduler = test_scheduler().await;
    let peer_a = ProjectId::now_v7();
    let peer_b = ProjectId::now_v7();

    register(
        &scheduler,
        "w1",
        eval_worker_caps(),
        HashSet::from([peer_a]),
    )
    .await;

    scheduler
        .enqueue_eval_job("ja".into(), eval_job(peer_a))
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("jb".into(), eval_job(peer_b))
        .await
        .unwrap();

    assert_eq!(scheduler.get_job_candidates("w1").await.len(), 1);

    scheduler
        .update_authorized_peers("w1", HashSet::from([peer_a, peer_b]))
        .await;

    assert_eq!(scheduler.get_job_candidates("w1").await.len(), 2);
}

#[tokio::test]
async fn test_draining_worker_still_has_assigned_jobs() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();

    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();

    scheduler.request_job("w1", JobKind::Flake).await;
    scheduler.mark_worker_draining("w1").await;

    let workers = scheduler.workers_info().await;
    assert_eq!(workers.len(), 1);
    assert!(workers[0].draining);
    assert_eq!(workers[0].assigned_job_count, 1);
}

#[tokio::test]
async fn test_request_reauth_signals_connected_worker() {
    let scheduler = test_scheduler().await;
    let mut signals = register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler.request_reauth("w1").await;

    assert_eq!(signals.recv().await, Some(SessionSignal::Reauth));
}

#[tokio::test]
async fn abort_evaluation_signals_the_worker_running_its_job() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let mut signals = register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    let job = eval_job(peer);
    let eval_id = job.evaluation_id;
    scheduler.enqueue_eval_job("j1".into(), job).await.unwrap();
    assert_eq!(signals.recv().await, Some(SessionSignal::Offers(1)));
    let assigned = scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");
    assert_eq!(assigned.job_id(), "j1");
    assert_eq!(assigned.pending.evaluation_id(), eval_id);

    let aborted = scheduler.abort_evaluation_jobs(eval_id, vec![]).await;

    assert_eq!(aborted, vec![("w1".to_string(), "j1".to_string())]);
    assert_eq!(
        signals.recv().await,
        Some(SessionSignal::Abort {
            job_id: "j1".into(),
            reason: "evaluation aborted".into()
        })
    );
    assert_eq!(
        scheduler.counts().await.active,
        1,
        "the worker still runs it until it reports"
    );
}

#[tokio::test]
async fn aborting_an_evaluation_marks_it_and_stops_its_eval_job_itself() {
    use gradient_entity::evaluation::EvaluationStatus;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let ok = MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    };
    let job = eval_job(ProjectId::now_v7());
    let evaluation = gradient_types::MEvaluation {
        id: job.evaluation_id,
        status: EvaluationStatus::EvaluatingDerivation,
        ..Default::default()
    };
    let marked = gradient_types::MEvaluation {
        status: EvaluationStatus::Aborted,
        ..evaluation.clone()
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![ok; 16])
        .append_query_results(vec![vec![marked]; 8])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let mut signals = register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler.enqueue_eval_job("j1".into(), job).await.unwrap();
    assert_eq!(signals.recv().await, Some(SessionSignal::Offers(1)));
    scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");

    scheduler.abort_evaluation(evaluation).await;

    assert_eq!(
        signals.recv().await,
        Some(SessionSignal::Abort {
            job_id: "j1".into(),
            reason: "evaluation aborted".into()
        })
    );
    let log = log_db.into_transaction_log();
    let mark = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| s.sql.starts_with("UPDATE \"evaluation\" SET \"status\""))
        .expect("the evaluation is marked by the abort itself");
    assert!(
        format!("{:?}", mark.values).contains(&format!(
            "Int(Some({}))",
            i32::from(EvaluationStatus::Aborted)
        )),
        "{:?}",
        mark.values
    );
}

#[tokio::test]
async fn aborting_a_finished_evaluation_stops_nothing() {
    use gradient_entity::evaluation::EvaluationStatus;

    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let mut signals = register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    let job = eval_job(peer);
    let evaluation = gradient_types::MEvaluation {
        id: job.evaluation_id,
        status: EvaluationStatus::Completed,
        ..Default::default()
    };
    scheduler.enqueue_eval_job("j1".into(), job).await.unwrap();
    assert_eq!(signals.recv().await, Some(SessionSignal::Offers(1)));
    scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");

    scheduler.abort_evaluation(evaluation).await;

    assert!(signals.try_recv().is_err(), "no AbortJob was sent");
    assert_eq!(scheduler.counts().await.active, 1);
}

#[tokio::test]
async fn an_abort_the_worker_never_confirms_is_reaped_after_the_grace() {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let ok = MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![ok; 3])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    let job = eval_job(peer);
    let eval_id = job.evaluation_id;
    scheduler.enqueue_eval_job("j1".into(), job).await.unwrap();
    scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");
    scheduler.abort_evaluation_jobs(eval_id, vec![]).await;

    let reaped = scheduler
        .reap_overdue_aborts(std::time::Duration::ZERO)
        .await;

    assert_eq!(reaped, vec!["j1".to_owned()]);
    assert_eq!(scheduler.counts().await.active, 0);
    assert_eq!(scheduler.workers_info().await[0].assigned_job_count, 0);
    let log = log_db.into_transaction_log();
    let close = log
        .iter()
        .flat_map(|t| t.statements())
        .filter(|s| s.sql.starts_with("UPDATE \"dispatched_job\""))
        .find(|s| format!("{:?}", s.values).contains("\"j1\""))
        .expect("the reaped job's dispatch row is closed");
    assert!(
        close.sql.contains("\"finished_at\" IS NULL"),
        "{}",
        close.sql
    );
}

#[tokio::test]
async fn a_running_job_nobody_aborted_is_never_reaped() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();
    scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");

    let reaped = scheduler
        .reap_overdue_aborts(std::time::Duration::ZERO)
        .await;

    assert!(reaped.is_empty());
    assert_eq!(scheduler.counts().await.active, 1);
}

/// Two live evaluations building the same derivation are sharing one global shared build. The
/// scheduler must stop exactly the shared builds the database abort moved. Aborting by evaluation
/// alone would kill a build the other evaluation is waiting on.
#[tokio::test]
async fn abort_leaves_a_shared_build_running_for_the_other_evaluation() {
    use crate::jobs::{PendingJob, build_job_key};

    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let aborted_eval = EvaluationId::now_v7();
    let shared = DerivationBuildId::now_v7();
    let only_mine = DerivationBuildId::now_v7();

    let (session, mut signals) = port();
    scheduler
        .reattach_worker(
            "w1",
            build_worker_caps(),
            HashSet::new(),
            session,
            vec![
                crate::jobs::Reattached::single(
                    build_job_key(shared),
                    PendingJob::Build(build_job(aborted_eval, peer, shared)),
                ),
                crate::jobs::Reattached::single(
                    build_job_key(only_mine),
                    PendingJob::Build(build_job(aborted_eval, peer, only_mine)),
                ),
            ],
        )
        .await
        .expect("reattach");

    let aborted = scheduler
        .abort_evaluation_jobs(aborted_eval, vec![only_mine])
        .await;

    assert_eq!(aborted, vec![("w1".to_string(), build_job_key(only_mine))]);
    assert_eq!(
        signals.recv().await,
        Some(SessionSignal::Abort {
            job_id: build_job_key(only_mine),
            reason: "evaluation aborted".into()
        })
    );
    assert!(
        scheduler.active_job(&build_job_key(shared)).await.is_some(),
        "the shared build keeps building for the evaluation that still needs it"
    );
}

#[tokio::test]
async fn abort_stops_a_shared_build_dispatched_for_an_evaluation_aborted_earlier() {
    use crate::jobs::{PendingJob, build_job_key};

    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let dispatched_for = EvaluationId::now_v7();
    let last_wanting = EvaluationId::now_v7();
    let shared = DerivationBuildId::now_v7();

    let (session, mut signals) = port();
    scheduler
        .reattach_worker(
            "w1",
            build_worker_caps(),
            HashSet::new(),
            session,
            vec![crate::jobs::Reattached::single(
                build_job_key(shared),
                PendingJob::Build(build_job(dispatched_for, peer, shared)),
            )],
        )
        .await
        .expect("reattach");

    let aborted = scheduler
        .abort_evaluation_jobs(last_wanting, vec![shared])
        .await;

    assert_eq!(aborted, vec![("w1".to_string(), build_job_key(shared))]);
    assert_eq!(
        signals.recv().await,
        Some(SessionSignal::Abort {
            job_id: build_job_key(shared),
            reason: "evaluation aborted".into()
        })
    );
}

#[tokio::test]
async fn cancelling_an_evaluation_aborts_its_running_jobs_instead_of_forgetting_them() {
    use crate::jobs::{PendingJob, build_job_key};

    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let eval_id = EvaluationId::now_v7();
    let shared = DerivationBuildId::now_v7();

    let (session, mut signals) = port();
    scheduler
        .reattach_worker(
            "w1",
            build_worker_caps(),
            HashSet::new(),
            session,
            vec![crate::jobs::Reattached::single(
                build_job_key(shared),
                PendingJob::Build(build_job(eval_id, peer, shared)),
            )],
        )
        .await
        .expect("reattach");

    scheduler.cancel_evaluation_jobs(eval_id, &[shared]).await;

    assert_eq!(
        signals.recv().await,
        Some(SessionSignal::Abort {
            job_id: build_job_key(shared),
            reason: "evaluation aborted".into()
        })
    );
    assert!(
        scheduler.active_job(&build_job_key(shared)).await.is_some(),
        "the job stays tracked until the worker confirms, so the reaper can still close it"
    );
}

#[tokio::test]
async fn record_eval_message_drops_when_job_unknown() {
    let scheduler = test_scheduler().await;
    let r = scheduler
        .record_eval_message(
            "ghost-job",
            gradient_wire::types::EvalMessageLevel::Error,
            "build-prefetch".into(),
            "nope".into(),
        )
        .await;
    assert!(r.is_ok(), "missing active job must not be an error");
}

#[tokio::test]
async fn record_eval_message_inserts_for_active_build_job() {
    use gradient_test_support::prelude::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let eval_id = EvaluationId::now_v7();
    let peer = ProjectId::now_v7();
    let build_id = DerivationBuildId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let state = test_state(db);
    let scheduler = Arc::new(Scheduler::new(state));
    scheduler.spawn_core(None).await.expect("core actor");

    scheduler
        .enqueue_build_job("jbuild".into(), build_job(eval_id, peer, build_id))
        .await
        .unwrap();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "jbuild".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        )
        .await;
    scheduler.request_job("w1", JobKind::Build).await;

    scheduler
        .record_eval_message(
            "jbuild",
            gradient_wire::types::EvalMessageLevel::Error,
            "build-prefetch".into(),
            "input prefetch failed: no nar_hash".into(),
        )
        .await
        .expect("insert should succeed");
}

#[tokio::test]
async fn fetch_only_completion_enqueues_cached_eval_followup() {
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_test_support::prelude::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    let eval_id = EvaluationId::now_v7();
    let peer = ProjectId::now_v7();
    let source_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source";

    let archived_eval = gradient_entity::evaluation::Model {
        id: eval_id,
        repository: "https://example.com/repo".into(),
        commit: CommitId::nil(),
        wildcard: "*".into(),
        status: EvaluationStatus::Building,
        created_at: gradient_types::now(),
        updated_at: gradient_types::now(),
        flake_source: Some(source_path.into()),
        ..Default::default()
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![archived_eval]])
        .into_connection();
    let scheduler = Arc::new(Scheduler::new(test_state(db)));
    scheduler.spawn_core(None).await.expect("core actor");

    let mut fetch_job = eval_job(peer);
    fetch_job.evaluation_id = eval_id;
    fetch_job.job.steps = vec![FlakeStep::FetchFlake];
    let job_id = format!("eval:{eval_id}");

    let (session, _signals) = port();
    scheduler
        .reattach_worker(
            "w1",
            GradientCapabilities {
                eval: true,
                fetch: true,
                ..GradientCapabilities::default()
            },
            HashSet::new(),
            session,
            vec![crate::jobs::Reattached::single(
                job_id.clone(),
                crate::jobs::PendingJob::Eval(fetch_job),
            )],
        )
        .await
        .expect("reattach");

    scheduler
        .handle_job_completed("w1", &job_id)
        .await
        .expect("fetch-only completion should succeed");

    assert_eq!(scheduler.pending_job_count().await, 1);

    let follow = match scheduler
        .pending_job(&job_id)
        .await
        .expect("follow-up must be pending")
    {
        crate::jobs::PendingJob::Eval(e) => e,
        other => panic!("expected an eval follow-up, got {other:?}"),
    };
    match &follow.job.source {
        gradient_wire::types::FlakeSource::Cached { store_path } => {
            assert_eq!(store_path, source_path)
        }
        other => panic!("expected Cached source, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_evaluation_jobs_drops_eval_and_build_jobs() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let eval_id = EvaluationId::now_v7();
    let build_id_a = DerivationBuildId::now_v7();
    let build_id_b = DerivationBuildId::now_v7();

    scheduler
        .enqueue_eval_job(
            format!("eval:{eval_id}"),
            PendingEvalJob {
                evaluation_id: eval_id,
                task_id: None,
                project_id: peer,
                commit_id: CommitId::now_v7(),
                repository: "https://example.com/repo".into(),
                job: gradient_wire::types::FlakeJob {
                    steps: vec![gradient_wire::types::FlakeStep::EvaluateDerivations],
                    source: gradient_wire::types::FlakeSource::Repository {
                        url: "https://example.com/repo".into(),
                        commit: "abc123".into(),
                    },
                    wildcards: vec!["*".into()],
                    timeout_secs: None,
                    input_overrides: vec![],
                    input_update: None,
                },
                required_paths: vec![],
                queued_at: gradient_types::now(),
                ready_at: gradient_types::now(),
                rescore_count: 0,
                prioritized: false,
                build_request: false,
                history: Default::default(),
                walk_mode: Default::default(),
            },
        )
        .await
        .unwrap();

    for (build_id, job_id) in [
        (build_id_a, format!("build:{build_id_a}")),
        (build_id_b, format!("build:{build_id_b}")),
    ] {
        scheduler
            .enqueue_build_job(job_id, build_job(eval_id, peer, build_id))
            .await
            .unwrap();
    }

    assert_eq!(scheduler.pending_job_count().await, 3);

    scheduler
        .cancel_evaluation_jobs(eval_id, &[build_id_a, build_id_b])
        .await;

    let ids = vec![
        format!("eval:{eval_id}"),
        format!("build:{build_id_a}"),
        format!("build:{build_id_b}"),
    ];
    assert_eq!(scheduler.untracked(ids.clone()).await, ids);
    let counts = scheduler.counts().await;
    assert_eq!(counts.pending + counts.active, 0);
}

#[tokio::test]
async fn a_respawned_core_is_rebuilt_from_reattached_sessions() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let (session, mut signals) = port();
    scheduler
        .register_worker(
            "w1",
            eval_worker_caps(),
            HashSet::new(),
            Arc::clone(&session),
        )
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();
    assert_eq!(signals.recv().await, Some(SessionSignal::Offers(1)));
    let assigned = scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");

    let old = scheduler
        .core_changes()
        .borrow()
        .clone()
        .expect("core published");
    old.stop_and_wait(None, None)
        .await
        .expect("stop the old core");
    scheduler.spawn_core(None).await.expect("respawn");
    assert_eq!(
        scheduler.counts().await.active,
        0,
        "a fresh core knows nothing"
    );

    scheduler
        .reattach_worker(
            "w1",
            eval_worker_caps(),
            HashSet::new(),
            session,
            vec![crate::jobs::Reattached::single(
                assigned.job_id().to_owned(),
                assigned.pending.clone(),
            )],
        )
        .await
        .unwrap();

    let counts = scheduler.counts().await;
    assert_eq!((counts.workers, counts.active, counts.pending), (1, 1, 0));
    assert!(scheduler.active_job(assigned.job_id()).await.is_some());
    assert!(scheduler.is_worker_connected("w1").await);
}

#[tokio::test]
async fn registering_a_worker_closes_only_the_rows_it_never_assigned() {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 2,
        }])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;

    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    let log = log_db.into_transaction_log();
    let close = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| s.sql.starts_with("UPDATE \"dispatched_job\""))
        .expect("registration closes the worker's open rows");
    assert!(
        close.sql.contains("\"worker_id\" = $3") && close.sql.contains("\"finished_at\" IS NULL"),
        "{}",
        close.sql
    );
    assert!(
        close.sql.contains("\"dispatched_at\" <"),
        "a blanket close by worker id rewrites the report that is still landing: {}",
        close.sql
    );
    let values = format!("{:?}", close.values);
    assert!(values.contains("\"w1\""), "{values}");
    assert!(
        values.contains(&format!("{:?}", scheduler.state.started_at.naive_utc())),
        "{values}"
    );
}

#[tokio::test]
async fn the_assignment_record_is_written_before_the_assignment_returns() {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let ok = MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    };
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(vec![ok; 2])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();

    let assigned = scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("assigned");

    let log = log_db.into_transaction_log();
    let insert = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| s.sql.starts_with("INSERT INTO \"dispatched_job\""))
        .expect("the dispatched_job insert ran before request_job returned");
    let values = format!("{:?}", insert.values);
    assert!(values.contains("\"j1\""), "{values}");
    assert!(
        values.contains(&assigned.assignment_id().to_string()),
        "{values}"
    );
}

fn claim_results(won: &[bool]) -> sea_orm::DatabaseConnection {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let registration = MockExecResult {
        last_insert_id: 0,
        rows_affected: 0,
    };
    let claims = won.iter().map(|won| MockExecResult {
        last_insert_id: 0,
        rows_affected: u64::from(*won),
    });
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results(std::iter::once(registration).chain(claims))
        .into_connection()
}

#[tokio::test]
async fn a_lost_claim_drops_the_job_and_claims_the_next() {
    let scheduler = test_scheduler_with(claim_results(&[false, true])).await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    for id in ["j1", "j2"] {
        scheduler
            .enqueue_eval_job(id.into(), eval_job(peer))
            .await
            .unwrap();
    }

    let assigned = scheduler
        .request_job("w1", JobKind::Flake)
        .await
        .expect("the second claim wins");

    assert_eq!(assigned.job_id(), "j2");
    assert!(scheduler.pending_job("j1").await.is_none());
    assert!(scheduler.active_job("j1").await.is_none());
}

#[tokio::test]
async fn a_lost_build_claim_hands_its_shared_build_back_to_the_startable_set() {
    let scheduler = test_scheduler_with(claim_results(&[false])).await;
    let peer = ProjectId::now_v7();
    let job = build_job(EvaluationId::now_v7(), peer, DerivationBuildId::now_v7());
    let derivation = job.derivation;
    register(&scheduler, "w1", build_worker_caps(), HashSet::new()).await;
    scheduler
        .update_worker_capabilities(
            "w1",
            WorkerCapabilities {
                architectures: vec![job.job.requirement.architecture.clone()],
                system_features: vec![],
                max_concurrent_builds: 1,
                cpu_count: 8,
                ram_total_mb: 16_000,
                cpu_core_score: 100,
                ..Default::default()
            },
        )
        .await;
    scheduler
        .enqueue_build_job("jbuild".into(), job)
        .await
        .unwrap();
    scheduler
        .record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "jbuild".into(),
                missing_count: 0,
                missing_nar_size: 0,
                outputs_present: false,
            }],
        )
        .await;

    assert!(scheduler.request_job("w1", JobKind::Build).await.is_none());

    assert!(scheduler.active_job("jbuild").await.is_none());
    assert!(scheduler.pending_job("jbuild").await.is_none());
    assert_eq!(
        scheduler.state.startable_set.take().entered,
        HashSet::from([derivation])
    );
}

#[tokio::test]
async fn a_build_that_left_queued_leaves_the_tracker() {
    use gradient_entity::build::BuildStatus;
    use sea_orm::{DatabaseBackend, MockDatabase};

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let job = build_job(
        EvaluationId::now_v7(),
        ProjectId::now_v7(),
        DerivationBuildId::now_v7(),
    );
    let derivation = job.derivation;
    scheduler
        .enqueue_build_job("jbuild".into(), job)
        .await
        .unwrap();

    scheduler
        .state
        .startable_set
        .record(&[gradient_db::status::TransitionChange {
            derivation,
            from: BuildStatus::Queued,
            to: BuildStatus::Created,
        }]);
    crate::loops::admit_startable_moves(&scheduler)
        .await
        .expect("admission");

    assert!(scheduler.pending_job("jbuild").await.is_none());
    assert!(log_db.into_transaction_log().is_empty());
}

#[tokio::test]
async fn admission_reads_only_the_shared_builds_that_moved() {
    use gradient_entity::build::BuildStatus;
    use sea_orm::{DatabaseBackend, MockDatabase};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::derivation_build::Model>::new()])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let derivation = DerivationId::now_v7();

    scheduler
        .state
        .startable_set
        .record(&[gradient_db::status::TransitionChange {
            derivation,
            from: BuildStatus::Created,
            to: BuildStatus::Queued,
        }]);
    crate::loops::admit_startable_moves(&scheduler)
        .await
        .expect("admission");

    let log = log_db.into_transaction_log();
    let statements: Vec<_> = log.iter().flat_map(|t| t.statements()).collect();
    assert_eq!(statements.len(), 1);
    assert!(
        statements[0]
            .sql
            .contains("db.derivation = ANY($1::uuid[])"),
        "{}",
        statements[0].sql
    );
    let values = format!("{:?}", statements[0].values);
    assert!(values.contains(&derivation.to_string()), "{values}");
}

#[tokio::test]
async fn the_resync_prunes_pending_builds_no_longer_startable() {
    use sea_orm::{DatabaseBackend, MockDatabase};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::derivation_build::Model>::new()])
        .into_connection();
    let scheduler = test_scheduler_with(db).await;
    scheduler
        .enqueue_build_job(
            "jbuild".into(),
            build_job(
                EvaluationId::now_v7(),
                ProjectId::now_v7(),
                DerivationBuildId::now_v7(),
            ),
        )
        .await
        .unwrap();
    scheduler
        .enqueue_eval_job("jeval".into(), eval_job(ProjectId::now_v7()))
        .await
        .unwrap();

    crate::loops::resync_startable_set(&scheduler)
        .await
        .expect("resync");

    assert!(scheduler.pending_job("jbuild").await.is_none());
    assert!(
        scheduler.pending_job("jeval").await.is_some(),
        "evaluations are not the build startable set's to prune"
    );
}

#[tokio::test]
async fn a_failed_assignment_record_withdraws_the_assignment() {
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .append_exec_errors([DbErr::Custom("dispatched_job insert refused".into())])
        .into_connection();
    let scheduler = test_scheduler_with(db).await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();

    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_none());

    assert_eq!(scheduler.pending_job_count().await, 1);
    assert!(scheduler.active_job("j1").await.is_none());
    assert!(scheduler.pending_job("j1").await.is_some());
}

fn dispatched_row(id: DispatchedJobId) -> gradient_entity::dispatched_job::Model {
    gradient_entity::dispatched_job::Model {
        id,
        worker_id: "w1".into(),
        job_id: Some("j1".into()),
        evaluation_id: EvaluationId::now_v7(),
        project: ProjectId::now_v7(),
        queued_at: gradient_types::now(),
        dispatched_at: gradient_types::now(),
        created_at: gradient_types::now(),
        ..Default::default()
    }
}

fn closed_dispatched_row(id: DispatchedJobId) -> gradient_entity::dispatched_job::Model {
    use gradient_entity::dispatched_job::DispatchedJobOutcome;

    gradient_entity::dispatched_job::Model {
        finished_at: Some(gradient_types::now()),
        outcome: Some(DispatchedJobOutcome::Abandoned),
        ..dispatched_row(id)
    }
}

#[tokio::test]
async fn a_report_without_an_assignment_row_is_dropped_loudly() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, MockDatabase};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([Vec::<gradient_entity::dispatched_job::Model>::new()])
        .into_connection();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(
            DispatchedJobId::now_v7(),
            DispatchedJobOutcome::Completed,
            ReportedTimeline::received(vec![], 0),
        )
        .await;

    assert_eq!(landing, TimelineLanding::NoRow);
}

#[tokio::test]
async fn a_failed_lookup_is_not_reported_as_a_missing_row() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors([DbErr::Custom("lookup refused".into())])
        .into_connection();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(
            DispatchedJobId::now_v7(),
            DispatchedJobOutcome::Completed,
            ReportedTimeline::received(vec![], 0),
        )
        .await;

    assert_eq!(landing, TimelineLanding::LookupFailed);
}

#[tokio::test]
async fn a_report_with_an_open_row_closes_it() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, MockDatabase};

    let assignment_id = DispatchedJobId::now_v7();
    let row = dispatched_row(assignment_id);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row.clone()], vec![row]])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(
            assignment_id,
            DispatchedJobOutcome::Completed,
            ReportedTimeline::received(vec![], 0),
        )
        .await;

    assert_eq!(landing, TimelineLanding::Closed);
    let log = log_db.into_transaction_log();
    let close = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| s.sql.starts_with("UPDATE \"dispatched_job\""))
        .expect("the report closes its own row");
    let set = close
        .sql
        .split(" RETURNING ")
        .next()
        .expect("the update has a body");
    assert!(
        set.contains("\"finished_at\" =") && set.contains("\"outcome\" ="),
        "the stamp must be in the SET, not only echoed by RETURNING: {}",
        close.sql
    );
    assert!(format!("{:?}", close.values).contains(&assignment_id.to_string()));
}

#[tokio::test]
async fn a_closing_report_stores_its_arrival_and_the_worker_clock() {
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};

    let assignment_id = DispatchedJobId::now_v7();
    let row = dispatched_row(assignment_id);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row.clone()], vec![row]])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let received_at = gradient_types::now() - chrono::Duration::seconds(30);

    scheduler
        .persist_job_timeline(
            assignment_id,
            DispatchedJobOutcome::Completed,
            ReportedTimeline {
                spans: vec![],
                worker_elapsed_ms: 4_321,
                received_at,
            },
        )
        .await;

    let log = log_db.into_transaction_log();
    let close = log
        .iter()
        .flat_map(|t| t.statements())
        .find(|s| s.sql.starts_with("UPDATE \"dispatched_job\""))
        .expect("the report closes its own row");
    assert!(
        close.sql.contains("\"worker_elapsed_ms\" ="),
        "{}",
        close.sql
    );
    let values = &close.values.as_ref().expect("bound values").0;
    assert!(values.contains(&Value::BigInt(Some(4_321))), "{values:?}");
    assert!(
        values.contains(&Value::ChronoDateTime(Some(received_at))),
        "{values:?}"
    );
}

#[tokio::test]
async fn a_close_that_fails_is_reported_as_a_failed_close() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    let assignment_id = DispatchedJobId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![dispatched_row(assignment_id)]])
        .append_query_errors([DbErr::Custom("close refused".into())])
        .into_connection();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(
            assignment_id,
            DispatchedJobOutcome::Completed,
            ReportedTimeline::received(vec![], 0),
        )
        .await;

    assert_eq!(landing, TimelineLanding::CloseFailed);
}

#[tokio::test]
async fn a_report_for_an_already_closed_row_keeps_the_recorded_outcome() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use gradient_wire::types::{JobPhase, JobPhaseSpan};
    use sea_orm::{DatabaseBackend, MockDatabase};

    let assignment_id = DispatchedJobId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![closed_dispatched_row(assignment_id)]])
        .append_query_results([vec![gradient_entity::dispatched_job_phase::Model {
            id: DispatchedJobPhaseId::now_v7(),
            dispatched_job: assignment_id,
            ..Default::default()
        }]])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(
            assignment_id,
            DispatchedJobOutcome::Completed,
            ReportedTimeline::received(
                vec![JobPhaseSpan {
                    phase: JobPhase::Fetch,
                    start_ms: 0,
                    end_ms: 10,
                    ..Default::default()
                }],
                10,
            ),
        )
        .await;

    assert_eq!(landing, TimelineLanding::AlreadyClosed);
    let log = log_db.into_transaction_log();
    let sql: Vec<&str> = log
        .iter()
        .flat_map(|t| t.statements())
        .map(|s| s.sql.as_str())
        .collect();
    assert!(
        !sql.iter()
            .any(|s| s.starts_with("UPDATE \"dispatched_job\"")),
        "{sql:?}"
    );
    assert!(
        sql.iter()
            .any(|s| s.starts_with("INSERT INTO \"dispatched_job_phase\"")),
        "{sql:?}"
    );
    assert!(
        !sql.iter()
            .any(|s| s.starts_with("UPDATE \"evaluation_metric\"")),
        "the totals key on the evaluation, so a superseded dispatch must not write them: {sql:?}"
    );

    let lookup = sql
        .iter()
        .find(|s| s.starts_with("SELECT"))
        .expect("the report looks its row up");
    assert!(
        !lookup.contains("\"finished_at\" IS NULL"),
        "the lookup must see a closed row too, or the already-closed path is unreachable: {lookup}"
    );
}

fn statements(log: &[sea_orm::Transaction]) -> Vec<String> {
    log.iter()
        .flat_map(|t| t.statements())
        .map(|s| s.sql.clone())
        .collect()
}

#[tokio::test]
async fn registering_opens_a_connection_row_without_a_worker_registration() {
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;
    let mut events = scheduler.state.events.subscribe();
    let peers = HashSet::from([ProjectId::now_v7(), ProjectId::now_v7()]);

    register(&scheduler, "w1", eval_worker_caps(), peers.clone()).await;

    let sql = statements(&log_db.into_transaction_log());
    assert!(
        sql.iter()
            .any(|s| s.starts_with("INSERT INTO \"worker_connection\"")),
        "{sql:#?}"
    );
    assert!(
        !sql.iter().any(|s| s.contains("worker_registration")),
        "{sql:#?}"
    );
    let announced = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|env| match &env.event {
            gradient_types::Event::WorkerConnected(w) => Some(w.projects.clone()),
            _ => None,
        })
        .expect("the connection is announced");
    let expected: HashSet<ProjectId> = peers.into_iter().collect();
    assert_eq!(announced.into_iter().collect::<HashSet<_>>(), expected);
}

#[tokio::test]
async fn every_connected_worker_is_sampled() {
    use sea_orm::{DatabaseBackend, MockDatabase};

    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    let workers = scheduler.board_workers().await;
    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let log_db = db.clone();

    for info in &workers {
        crate::worker_lifecycle::record_worker_sample(&db, info).await;
    }

    let sql = statements(&log_db.into_transaction_log());
    assert_eq!(
        sql.iter()
            .filter(|s| s.starts_with("INSERT INTO \"worker_sample\""))
            .count(),
        1,
        "{sql:#?}"
    );
}

fn member_of(
    cluster: gradient_types::ids::ClusterJobId,
    count: u32,
) -> gradient_db::scheduling::cluster::MemberOf {
    crate::cluster::book::book_tests::member_of(cluster, count)
}

#[tokio::test]
async fn an_empty_answer_records_an_idle_slot_until_a_job_fills_it() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_none());
    let idle = scheduler.cluster_snapshot().await.slots;
    assert_eq!(idle.len(), 1);
    assert_eq!(
        (idle[0].worker.as_str(), idle[0].kind),
        ("w1", crate::cluster::SlotKind::Eval)
    );

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await
        .unwrap();
    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_some());
    assert!(scheduler.cluster_snapshot().await.slots.is_empty());
}

#[tokio::test]
async fn a_member_waits_until_its_cluster_is_whole() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let cluster = gradient_types::ids::ClusterJobId::now_v7();
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    scheduler
        .enqueue_cluster_member(
            member_of(cluster, 2),
            "eval:a".into(),
            crate::jobs::PendingJob::Eval(eval_job(peer)),
        )
        .await
        .unwrap();
    assert!(scheduler.cluster_snapshot().await.clusters.is_empty());
    assert!(scheduler.untracked(vec!["eval:a".into()]).await.is_empty());
    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_none());

    scheduler
        .enqueue_cluster_member(
            member_of(cluster, 2),
            "eval:b".into(),
            crate::jobs::PendingJob::Eval(eval_job(peer)),
        )
        .await
        .unwrap();
    let snapshot = scheduler.cluster_snapshot().await;
    assert_eq!(snapshot.clusters.len(), 1);
    assert_eq!(snapshot.clusters[0].members.len(), 2);
    assert_eq!(scheduler.pending_job_count().await, 0);
}

#[tokio::test]
async fn a_disconnect_drops_the_workers_idle_slots() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_none());

    scheduler.unregister_worker("w1").await;

    assert!(scheduler.cluster_snapshot().await.slots.is_empty());
}

#[tokio::test]
async fn a_worker_that_cannot_build_has_no_idle_build_slot() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;

    assert!(scheduler.request_job("w1", JobKind::Build).await.is_none());

    assert!(scheduler.cluster_snapshot().await.slots.is_empty());
}

#[tokio::test]
async fn a_draining_worker_offers_no_idle_slot() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_none());

    scheduler.mark_worker_draining("w1").await;

    assert!(scheduler.cluster_snapshot().await.slots.is_empty());
}

#[tokio::test]
async fn the_snapshot_lists_the_oldest_cluster_first() {
    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let (older, newer) = (
        gradient_types::ids::ClusterJobId::now_v7(),
        gradient_types::ids::ClusterJobId::now_v7(),
    );
    for (cluster, age) in [(newer, 1), (older, 60)] {
        let mut of = member_of(cluster, 1);
        of.cluster.created_at = gradient_types::now() - chrono::Duration::seconds(age);
        scheduler
            .enqueue_cluster_member(
                of,
                format!("eval:{cluster}"),
                crate::jobs::PendingJob::Eval(eval_job(peer)),
            )
            .await
            .unwrap();
    }

    let ids: Vec<_> = scheduler
        .cluster_snapshot()
        .await
        .clusters
        .iter()
        .map(|c| c.id)
        .collect();

    assert_eq!(ids, vec![older, newer]);
}

async fn ready_cluster(scheduler: &Scheduler, members: &[(&str, &str)]) -> ClusterJobId {
    let cluster = gradient_entity::cluster_job::Model {
        id: ClusterJobId::now_v7(),
        ..Default::default()
    };
    for (key, role) in members {
        let member = gradient_entity::cluster_member::Model {
            id: ClusterMemberId::now_v7(),
            cluster_job: cluster.id,
            evaluation: Some(EvaluationId::now_v7()),
            role: (*role).into(),
            ..Default::default()
        };
        let of = gradient_db::scheduling::cluster::MemberOf {
            cluster: cluster.clone(),
            member,
            member_count: members.len() as u32,
        };
        scheduler
            .enqueue_cluster_member(
                of,
                (*key).into(),
                crate::jobs::PendingJob::Eval(eval_job(ProjectId::now_v7())),
            )
            .await
            .unwrap();
    }

    cluster.id
}

async fn idle(scheduler: &Scheduler, worker: &str) -> mpsc::UnboundedReceiver<SessionSignal> {
    let rx = register(scheduler, worker, eval_worker_caps(), HashSet::new()).await;
    assert!(
        scheduler
            .request_job(worker, JobKind::Flake)
            .await
            .is_none()
    );

    rx
}

async fn placed(scheduler: &Scheduler) -> crate::cluster::Placement {
    let snapshot = scheduler.cluster_snapshot().await;
    crate::cluster::plan(&snapshot.clusters[0], &snapshot.slots, &snapshot.scores).expect("placed")
}

#[tokio::test]
async fn a_placement_takes_the_cluster_and_its_seats() {
    let scheduler = test_scheduler().await;
    let _w1 = idle(&scheduler, "w1").await;
    let _w2 = idle(&scheduler, "w2").await;
    let cluster = ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    let placement = placed(&scheduler).await;

    let committing = scheduler
        .take_placement(placement, ClusterAttemptId::now_v7())
        .await
        .expect("taken");

    assert_eq!(committing.cluster.id, cluster);
    assert_eq!(committing.seats.len(), 2);
    assert!(scheduler.cluster_snapshot().await.clusters.is_empty());
    assert!(scheduler.active_job("eval:a").await.is_some());
}

#[tokio::test]
async fn a_placement_whose_worker_went_busy_is_refused() {
    let scheduler = test_scheduler().await;
    let _w1 = idle(&scheduler, "w1").await;
    let _w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    let placement = placed(&scheduler).await;
    scheduler
        .enqueue_eval_job("eval:single".into(), eval_job(ProjectId::now_v7()))
        .await
        .unwrap();
    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_some());

    assert!(
        scheduler
            .take_placement(placement, ClusterAttemptId::now_v7())
            .await
            .is_none()
    );
    assert_eq!(scheduler.cluster_snapshot().await.clusters.len(), 1);
}

#[tokio::test]
async fn a_restored_placement_waits_again() {
    let scheduler = test_scheduler().await;
    let _w1 = idle(&scheduler, "w1").await;
    let _w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    let placement = placed(&scheduler).await;
    let committing = scheduler
        .take_placement(placement, ClusterAttemptId::now_v7())
        .await
        .expect("taken");

    scheduler.restore_cluster(committing).await;

    assert_eq!(scheduler.cluster_snapshot().await.clusters.len(), 1);
    assert!(scheduler.active_job("eval:a").await.is_none());
}

fn cluster_claims(results: &[u64]) -> sea_orm::DatabaseConnection {
    sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
        .append_query_results(
            (0..16).map(|_| Vec::<std::collections::BTreeMap<&str, sea_orm::Value>>::new()),
        )
        .append_exec_results(
            [0, 0]
                .iter()
                .chain(results)
                .map(|&n| sea_orm::MockExecResult {
                    last_insert_id: 0,
                    rows_affected: n,
                }),
        )
        .into_connection()
}

fn drain(rx: &mut mpsc::UnboundedReceiver<SessionSignal>) -> Vec<SessionSignal> {
    let mut signals = Vec::new();
    while let Ok(s) = rx.try_recv() {
        signals.push(s);
    }
    signals
}

fn assigned(signals: &[SessionSignal]) -> Option<String> {
    signals.iter().find_map(|s| match s {
        SessionSignal::ClusterAssign { job_id } => Some(job_id.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn a_ready_cluster_on_idle_workers_is_assigned_then_started() {
    let scheduler = test_scheduler_with(cluster_claims(&[1, 1, 1, 1, 1])).await;
    let mut w1 = idle(&scheduler, "w1").await;
    let mut w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;

    scheduler.plan_clusters().await.unwrap();

    let j1 = assigned(&drain(&mut w1)).expect("w1 assigned");
    let j2 = assigned(&drain(&mut w2)).expect("w2 assigned");
    let first = scheduler.take_prepared(&j1).expect("prepared");
    assert_eq!(
        first.membership.hold_secs,
        30 + crate::cluster::CLUSTER_HOLD_MARGIN_SECS
    );
    assert!(scheduler.take_prepared(&j2).is_some());

    scheduler.cluster_member_accepted(&j1).await;
    assert!(
        !drain(&mut w1)
            .iter()
            .any(|s| matches!(s, SessionSignal::StartCluster { .. }))
    );
    scheduler.cluster_member_accepted(&j2).await;

    let roster = drain(&mut w1).into_iter().find_map(|s| match s {
        SessionSignal::StartCluster { roster, .. } => Some(roster),
        _ => None,
    });
    assert_eq!(roster.map(|r| r.len()), Some(2));
}

#[tokio::test]
async fn a_lost_cluster_claim_hands_the_members_back_to_the_feed() {
    let scheduler = test_scheduler_with(cluster_claims(&[1, 0])).await;
    let mut w1 = idle(&scheduler, "w1").await;
    let _w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;

    scheduler.plan_clusters().await.unwrap();

    assert!(assigned(&drain(&mut w1)).is_none());
    assert!(scheduler.active_job("eval:a").await.is_none());
    assert!(scheduler.cluster_snapshot().await.clusters.is_empty());
    assert_eq!(
        scheduler.untracked(vec!["eval:a".into()]).await,
        vec!["eval:a".to_owned()],
        "the startable feed re-reads a member whose cluster claim was lost"
    );
}

#[tokio::test]
async fn a_rejecting_member_aborts_the_attempt_and_returns_the_cluster() {
    let scheduler = test_scheduler_with(cluster_claims(&[1, 1, 1, 1, 2, 0])).await;
    let mut w1 = idle(&scheduler, "w1").await;
    let mut w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    scheduler.plan_clusters().await.unwrap();
    let j1 = assigned(&drain(&mut w1)).expect("w1 assigned");
    drain(&mut w2);

    assert!(scheduler.cluster_member_rejected(&j1).await);
    assert!(!scheduler.cluster_member_rejected(&j1).await);

    assert!(
        drain(&mut w2)
            .iter()
            .any(|s| matches!(s, SessionSignal::AbortCluster { .. }))
    );
    assert!(scheduler.active_job("eval:a").await.is_none());
    assert!(scheduler.untracked(vec!["eval:a".into()]).await.is_empty());
}

#[tokio::test]
async fn an_undelivered_member_times_the_attempt_out() {
    let mut state = gradient_test_support::prelude::test_state(cluster_claims(&[1, 1, 1, 1, 2, 0]));
    Arc::make_mut(&mut Arc::get_mut(&mut state).expect("unshared").config)
        .scheduler
        .cluster_prepare_timeout_secs = 0;
    let scheduler = Arc::new(Scheduler::new(state));
    scheduler.spawn_core(None).await.unwrap();
    let mut w1 = idle(&scheduler, "w1").await;
    let _w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    scheduler.plan_clusters().await.unwrap();
    drain(&mut w1);

    scheduler.plan_clusters().await.unwrap();

    assert!(
        drain(&mut w1)
            .iter()
            .any(|s| matches!(s, SessionSignal::AbortCluster { .. }))
    );
    assert!(scheduler.active_job("eval:a").await.is_none());
    assert!(scheduler.untracked(vec!["eval:a".into()]).await.is_empty());
}

#[tokio::test]
async fn a_member_ending_before_its_attempt_started_fails_the_prepare() {
    let scheduler = test_scheduler_with(cluster_claims(&[1, 1, 1, 1, 2, 0])).await;
    let mut w1 = idle(&scheduler, "w1").await;
    let mut w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    scheduler.plan_clusters().await.unwrap();
    let j1 = assigned(&drain(&mut w1)).expect("w1 assigned");
    drain(&mut w2);

    assert!(scheduler.cluster_member_released(&j1).await);

    assert!(
        drain(&mut w2)
            .iter()
            .any(|s| matches!(s, SessionSignal::AbortCluster { .. }))
    );
    assert!(!scheduler.cluster_member_released(&j1).await);
}

#[tokio::test]
async fn one_pass_never_seats_two_clusters_on_one_worker() {
    let scheduler = test_scheduler_with(cluster_claims(&[1, 1, 1])).await;
    let _w1 = idle(&scheduler, "w1").await;
    let _w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    ready_cluster(&scheduler, &[("eval:c", "server"), ("eval:d", "client")]).await;

    scheduler.plan_clusters().await.unwrap();

    assert_eq!(scheduler.cluster_snapshot().await.clusters.len(), 1);
}

#[tokio::test]
async fn a_signal_reaches_the_other_members_of_a_started_attempt() {
    let scheduler = test_scheduler_with(cluster_claims(&[1, 1, 1, 1, 1])).await;
    let mut w1 = idle(&scheduler, "w1").await;
    let mut w2 = idle(&scheduler, "w2").await;
    ready_cluster(&scheduler, &[("eval:a", "server"), ("eval:b", "client")]).await;
    scheduler.plan_clusters().await.unwrap();
    let j1 = assigned(&drain(&mut w1)).expect("w1");
    let j2 = assigned(&drain(&mut w2)).expect("w2");
    let attempt = scheduler
        .take_prepared(&j1)
        .expect("prepared")
        .membership
        .attempt;
    scheduler.cluster_member_accepted(&j1).await;
    scheduler.cluster_member_accepted(&j2).await;
    drain(&mut w2);

    scheduler
        .forward_cluster_signal("w1", &attempt, None, b"hi".to_vec())
        .await;

    assert!(
        drain(&mut w2)
            .iter()
            .any(|s| matches!(s, SessionSignal::ClusterSignal { payload, .. } if payload == b"hi"))
    );
}

fn open_attempt(scheduler: &Scheduler, attempt: ClusterAttemptId, keys: &[&str]) {
    let cluster = ClusterJobId::now_v7();
    let members = keys
        .iter()
        .map(|key| crate::cluster::AttemptMember {
            job_id: (*key).into(),
            worker: "w1".into(),
            role: "node".into(),
            index: 0,
            primary: false,
            accepted: true,
            report: None,
            settled: false,
        })
        .collect();
    scheduler.attempts.lock().open(
        attempt,
        crate::cluster::AttemptState {
            cluster,
            parked: crate::cluster::PendingCluster {
                id: cluster,
                same_zone: false,
                queued_at: gradient_types::now(),
                expected: 0,
                members: Vec::new(),
                not_before: None,
            },
            members,
            roster: Vec::new(),
            deadline: std::time::Instant::now(),
            started: true,
            all_accepted: false,
            verdict: None,
            resolving: false,
            resolution: None,
            resolved_at: None,
        },
    );
}

#[tokio::test]
async fn an_attempt_member_counts_as_tracked() {
    let scheduler = test_scheduler().await;
    open_attempt(&scheduler, ClusterAttemptId::now_v7(), &["eval:a"]);

    let untracked = scheduler
        .untracked(vec!["eval:a".into(), "eval:b".into()])
        .await;

    assert_eq!(untracked, vec!["eval:b".to_owned()]);
}

#[tokio::test]
async fn a_reattached_member_is_not_requeued_as_a_single_job() {
    let scheduler = test_scheduler().await;
    let attempt = ClusterAttemptId::now_v7();
    let (session, _rx) = port();
    scheduler
        .reattach_worker(
            "w1",
            eval_worker_caps(),
            HashSet::new(),
            session,
            vec![crate::jobs::Reattached {
                job_id: "j1".into(),
                job: crate::jobs::PendingJob::Eval(eval_job(ProjectId::now_v7())),
                cluster: Some(attempt),
            }],
        )
        .await
        .unwrap();

    let disconnected = scheduler
        .call(|reply| crate::actor::SchedulerMsg::Unregister {
            worker: "w1".into(),
            reply,
        })
        .await
        .unwrap();

    assert!(disconnected.requeued.is_empty());
    assert_eq!(disconnected.cluster_members.len(), 1);
    assert_eq!(scheduler.pending_job_count().await, 0);
}

fn eval_reservation(worker: &str) -> crate::cluster::Reservation {
    crate::cluster::Reservation {
        placement: crate::cluster::Placement {
            cluster: gradient_entity::ids::ClusterJobId::now_v7(),
            seats: vec![crate::cluster::Seat {
                member: 0,
                worker: worker.into(),
            }],
        },
        kinds: vec![crate::cluster::SlotKind::Eval],
        since: std::time::Instant::now(),
    }
}

#[tokio::test]
async fn a_reserved_seat_gets_no_single_job() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .enqueue_eval_job("j1".into(), eval_job(ProjectId::now_v7()))
        .await
        .unwrap();
    assert!(scheduler.reserve(eval_reservation("w1")).await);

    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_none());
    assert_eq!(scheduler.pending_job_count().await, 1);
}

#[tokio::test]
async fn a_released_seat_takes_single_jobs_again() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .enqueue_eval_job("j1".into(), eval_job(ProjectId::now_v7()))
        .await
        .unwrap();
    let reservation = eval_reservation("w1");
    let cluster = reservation.cluster();
    assert!(scheduler.reserve(reservation).await);
    scheduler.release_reservation(cluster).await;

    assert!(scheduler.request_job("w1", JobKind::Flake).await.is_some());
}

#[tokio::test]
async fn a_second_reservation_is_refused() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    register(&scheduler, "w2", eval_worker_caps(), HashSet::new()).await;

    assert!(scheduler.reserve(eval_reservation("w1")).await);
    assert!(!scheduler.reserve(eval_reservation("w2")).await);
}

#[tokio::test]
async fn a_disconnected_seat_drops_the_reservation() {
    let scheduler = test_scheduler().await;
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    assert!(scheduler.reserve(eval_reservation("w1")).await);

    scheduler.unregister_worker("w1").await;

    assert!(scheduler.reservation().await.is_none());
}
