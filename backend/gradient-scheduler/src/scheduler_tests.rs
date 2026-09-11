/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the `Scheduler` - tests the coordination between
//! `WorkerPool` and `JobTracker` without requiring a real database.

use std::collections::HashSet;
use std::sync::Arc;

use gradient_types::ids::*;

use gradient_types::proto::{
    BuildJob, BuildSpec, CandidateScore, FlakeJob, FlakeStep, GradientCapabilities, JobKind,
};

use super::Scheduler;
use super::actor::{SessionPort, SessionSignal, WorkerCapabilities};
use super::jobs::{PendingBuildJob, PendingEvalJob};
use tokio::sync::mpsc;

/// A scheduler with its core actor running, backed by a mock DB whose queries
/// return nothing and whose exec buffer answers the scheduler's own writes.
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

/// `db` answers the scheduler's writes in order: one exec result per
/// registration (the worker's open rows close) and one per assignment (the
/// dispatch record).
async fn test_scheduler_with(db: sea_orm::DatabaseConnection) -> Arc<Scheduler> {
    use gradient_test_support::prelude::*;

    let state = test_state(db);
    let scheduler = Arc::new(Scheduler::new(state));
    scheduler.spawn_core(None).await.expect("core actor");
    scheduler
}

fn port() -> (Arc<dyn SessionPort>, mpsc::UnboundedReceiver<SessionSignal>) {
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

fn eval_job(peer: ProjectId) -> PendingEvalJob {
    PendingEvalJob {
        evaluation_id: EvaluationId::now_v7(),
        task_id: None,
        project_id: peer,
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            steps: vec![FlakeStep::EvaluateDerivations],
            source: gradient_types::proto::FlakeSource::Repository {
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
        history: Default::default(),
    }
}

/// One build job on the global anchor `derivation_build`, attributed to the
/// evaluation that dispatched it.
fn build_job(
    evaluation_id: EvaluationId,
    peer: ProjectId,
    derivation_build: DerivationBuildId,
) -> PendingBuildJob {
    PendingBuildJob {
        derivation_build,
        evaluation_id,
        project_id: peer,
        job: BuildJob {
            builds: vec![BuildSpec {
                build_id: derivation_build.to_string(),
                drv_path: "aaaa-hello.drv".into(),
                external_cached: false,
                is_fixed_output: false,
                outputs: vec![],
                timeout_secs: None,
                max_silent_secs: None,
            }],
        },
        required_paths: vec![],
        architecture: "x86_64-linux".into(),
        required_features: vec![],
        dependency_count: 0,
        closure_size: None,
        prefer_local_build: false,
        is_fixed_output: false,
        history: gradient_score::HistoryPrediction::default(),
        queued_at: gradient_types::now(),
        ready_at: gradient_types::now(),
        rescore_count: 0,
        pname: None,
        substitute: false,
    }
}

/// A dedicated eval worker for scheduling-mechanics tests that aren't about
/// capability gating: `eval` makes it eligible for the eval jobs they enqueue,
/// and the absence of `fetch` keeps the reserve-fetch-workers rule from
/// penalizing it into a negative score on the unscored request_job path.
fn eval_worker_caps() -> GradientCapabilities {
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

    // Open mode (empty authorized peers) → see all jobs.
    let candidates = scheduler.get_job_candidates("w1").await;
    assert_eq!(candidates.len(), 2);
}

/// Regression (#359): an enqueue must signal every active session, and the
/// generation it carries lets a session that missed the signal (busy on
/// something else) still fetch the delta on its next check instead of losing
/// the wakeup.
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

/// Reactive dispatch (#359): a kick advances the edge-trigger generation even
/// when no dispatcher is running yet, so the actor services it on its next
/// pass instead of losing the wakeup.
#[tokio::test]
async fn dispatch_kick_is_retained_when_not_awaiting() {
    use std::sync::atomic::Ordering;
    let scheduler = test_scheduler().await;
    let before = scheduler.kick_gen.load(Ordering::Relaxed);
    scheduler.kick_dispatch();
    assert_eq!(
        scheduler.kick_gen.load(Ordering::Relaxed),
        before + 1,
        "a kick must advance the generation the dispatcher compares against"
    );
}

/// A capability heartbeat must not reconcile inline: `update_worker_capabilities`
/// runs on the per-connection read loop, and awaiting the (DB-heavy) reconcile
/// there blocked the loop from reading the same worker's next `CacheQuery`, which
/// then timed out after 75s. It now kicks the dispatch loop, which reconciles off
/// that loop.
#[tokio::test]
async fn capability_update_kicks_dispatch_instead_of_reconciling_inline() {
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
            },
        )
        .await;
    assert_eq!(
        scheduler
            .kick_gen
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "capability update must kick the dispatch loop, not reconcile inline"
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

    // Worker scores the job, then explicitly requests one.
    scheduler
        .record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "j1".into(),
                missing_count: 0,
                missing_nar_size: 0,
            }],
        )
        .await;
    let assignment = scheduler.request_job("w1", JobKind::Flake).await;

    assert!(assignment.is_some());
    assert_eq!(assignment.unwrap().job_id, "j1");
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

    // Assign via RequestJob.
    scheduler.request_job("w1", JobKind::Flake).await;
    assert_eq!(scheduler.pending_job_count().await, 0);

    // Worker rejects the job → back to pending.
    scheduler.job_rejected("w1", "j1").await;
    assert_eq!(scheduler.pending_job_count().await, 1);
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

    // Assign both via RequestJob.
    scheduler.request_job("w1", JobKind::Flake).await;
    scheduler.request_job("w1", JobKind::Flake).await;

    assert_eq!(scheduler.pending_job_count().await, 0);

    // Worker disconnects → both jobs requeued.
    scheduler.unregister_worker("w1").await;
    assert_eq!(scheduler.pending_job_count().await, 2);
    assert_eq!(scheduler.worker_count().await, 0);

    // Another worker can pick them up.
    register(&scheduler, "w2", eval_worker_caps(), HashSet::new()).await;
    let candidates = scheduler.get_job_candidates("w2").await;
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn test_update_authorized_peers_expands_access() {
    let scheduler = test_scheduler().await;
    let peer_a = ProjectId::now_v7();
    let peer_b = ProjectId::now_v7();

    // Worker starts authorized for peer_a only.
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

    // Reauth adds peer_b.
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

    // Worker is draining but still has the assigned job.
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
async fn test_request_reauth_noop_for_disconnected_worker() {
    let scheduler = test_scheduler().await;
    scheduler.request_reauth("nonexistent").await;
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
    assert_eq!(assigned.job_id, "j1");
    assert_eq!(assigned.pending.evaluation_id(), eval_id);

    // An eval job has no anchor, so it stops on the evaluation alone.
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

/// Two live evaluations building the same derivation share its global anchor.
/// The database abort spares an anchor another live evaluation still holds a
/// `build_job` on and reports only the anchors it moved, so the scheduler must
/// stop exactly those: aborting by evaluation alone would kill a build the
/// other evaluation is still waiting on.
#[tokio::test]
async fn abort_leaves_a_shared_anchor_running_for_the_other_evaluation() {
    use crate::jobs::{PendingJob, build_job_key};

    let scheduler = test_scheduler().await;
    let peer = ProjectId::now_v7();
    let aborted_eval = EvaluationId::now_v7();
    let shared = DerivationBuildId::now_v7();
    let only_mine = DerivationBuildId::now_v7();

    // Both anchors are building on w1, dispatched by the evaluation being aborted.
    let (session, mut signals) = port();
    scheduler
        .reattach_worker(
            "w1",
            build_worker_caps(),
            HashSet::new(),
            session,
            vec![
                (
                    build_job_key(shared),
                    PendingJob::Build(build_job(aborted_eval, peer, shared)),
                ),
                (
                    build_job_key(only_mine),
                    PendingJob::Build(build_job(aborted_eval, peer, only_mine)),
                ),
            ],
        )
        .await
        .expect("reattach");

    // The other evaluation still needs `shared`, so the database abort left it
    // Building and reported only the anchor it took.
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
        "the shared anchor keeps building for the evaluation that still needs it"
    );
}

#[tokio::test]
async fn record_eval_message_drops_when_job_unknown() {
    // No active job → silently accepted, no DB insert attempted (MockDatabase
    // would panic on an unexpected exec; absence of panic proves no insert).
    let scheduler = test_scheduler().await;
    let r = scheduler
        .record_eval_message(
            "ghost-job",
            gradient_types::proto::EvalMessageLevel::Error,
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
    // Move to assigned so active_job() finds it. A zero-missing score clears the
    // negative-total dispatch gate.
    register(&scheduler, "w1", eval_worker_caps(), HashSet::new()).await;
    scheduler
        .record_scores(
            "w1",
            vec![CandidateScore {
                job_id: "jbuild".into(),
                missing_count: 0,
                missing_nar_size: 0,
            }],
        )
        .await;
    scheduler.request_job("w1", JobKind::Build).await;

    scheduler
        .record_eval_message(
            "jbuild",
            gradient_types::proto::EvalMessageLevel::Error,
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

    // Attach with the job already active: a dispatched assignment would draw an
    // exec result for the dispatch record that this buffer does not seed.
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
            vec![(job_id.clone(), crate::jobs::PendingJob::Eval(fetch_job))],
        )
        .await
        .expect("reattach");

    scheduler
        .handle_job_completed("w1", &job_id)
        .await
        .expect("fetch-only completion should succeed");

    assert_eq!(scheduler.pending_job_count().await, 1);

    // The follow-up reuses the `eval:{id}` id and carries a cached source. (We
    // inspect the pending tracker rather than dispatching, since under the new
    // negative-total gate a fetch-capable worker is reserved off cached-eval
    // work by ReserveFetchWorkersRule when spare capacity is unknown.)
    let follow = match scheduler
        .pending_job(&job_id)
        .await
        .expect("follow-up must be pending")
    {
        crate::jobs::PendingJob::Eval(e) => e,
        other => panic!("expected an eval follow-up, got {other:?}"),
    };
    match &follow.job.source {
        gradient_types::proto::FlakeSource::Cached { store_path } => {
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
                job: gradient_types::proto::FlakeJob {
                    steps: vec![gradient_types::proto::FlakeStep::EvaluateDerivations],
                    source: gradient_types::proto::FlakeSource::Repository {
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
                history: Default::default(),
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
            vec![(assigned.job_id.clone(), assigned.pending.clone())],
        )
        .await
        .unwrap();

    let counts = scheduler.counts().await;
    assert_eq!((counts.workers, counts.active, counts.pending), (1, 1, 0));
    assert!(scheduler.active_job(&assigned.job_id).await.is_some());
    assert!(scheduler.is_worker_connected("w1").await);
}

/// A fresh connection claims no job, so every row still open under that
/// worker belongs to a process that is gone. Closing them at registration
/// reopens the dispatch gate the moment the worker is back, instead of after
/// the abandoned sweep's grace.
#[tokio::test]
async fn registering_a_worker_closes_its_open_dispatch_rows() {
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
    assert!(format!("{:?}", close.values).contains("\"w1\""));
}

/// The row is the only proof a job is out, so it exists when the assignment
/// is handed back, not on a detached task the worker's first report can
/// overtake.
#[tokio::test]
async fn the_dispatch_record_is_written_before_the_assignment_returns() {
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
    assert!(values.contains(&assigned.dispatch.to_string()), "{values}");
}

/// A claim whose record cannot be written is released: the job is pending
/// again, nothing is active, and the worker gets no job to run unrecorded.
#[tokio::test]
async fn a_failed_dispatch_record_withdraws_the_assignment() {
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

/// With the record written before the job leaves, a report whose dispatch has
/// no row at all is a defect, not a race, so it is dropped with a warning
/// instead of returning silently.
#[tokio::test]
async fn a_report_without_a_dispatch_row_is_dropped_loudly() {
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
            vec![],
        )
        .await;

    assert_eq!(landing, TimelineLanding::NoRow);
}

/// A lookup that fails says nothing about whether the row exists, so it must
/// not be reported as the missing-row defect.
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
            vec![],
        )
        .await;

    assert_eq!(landing, TimelineLanding::LookupFailed);
}

/// The open row is stamped with this report's finish mark and outcome, keyed
/// on the dispatch the report carries.
#[tokio::test]
async fn a_report_with_an_open_row_closes_it() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, MockDatabase};

    let dispatch = DispatchedJobId::now_v7();
    let row = dispatched_row(dispatch);
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![row.clone()], vec![row]])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(dispatch, DispatchedJobOutcome::Completed, vec![])
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
    assert!(format!("{:?}", close.values).contains(&dispatch.to_string()));
}

/// A row that is found but cannot be stamped is not closed, so the report must
/// not claim it landed.
#[tokio::test]
async fn a_close_that_fails_is_reported_as_a_failed_close() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    let dispatch = DispatchedJobId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![dispatched_row(dispatch)]])
        .append_query_errors([DbErr::Custom("close refused".into())])
        .into_connection();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(dispatch, DispatchedJobOutcome::Completed, vec![])
        .await;

    assert_eq!(landing, TimelineLanding::CloseFailed);
}

/// Registration, the orphan requeue, the abandoned sweep and a withdrawn claim
/// all close a row a late report can still arrive for, so that is routine: the
/// outcome on record stands untouched while the phase timeline and the eval
/// totals, which key on the dispatch rather than on the close, still land.
#[tokio::test]
async fn a_report_for_an_already_closed_row_keeps_the_recorded_outcome() {
    use crate::job_handlers::timeline::TimelineLanding;
    use gradient_entity::dispatched_job::DispatchedJobOutcome;
    use gradient_types::proto::{JobPhase, JobPhaseSpan};
    use sea_orm::{DatabaseBackend, MockDatabase};

    let dispatch = DispatchedJobId::now_v7();
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![closed_dispatched_row(dispatch)]])
        .append_query_results([vec![gradient_entity::dispatched_job_phase::Model {
            id: DispatchedJobPhaseId::now_v7(),
            dispatched_job: dispatch,
            ..Default::default()
        }]])
        .into_connection();
    let log_db = db.clone();
    let scheduler = test_scheduler_with(db).await;

    let landing = scheduler
        .persist_job_timeline(
            dispatch,
            DispatchedJobOutcome::Completed,
            vec![JobPhaseSpan {
                phase: JobPhase::Fetch,
                start_ms: 0,
                end_ms: 10,
                ..Default::default()
            }],
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
