/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for the `Scheduler` - tests the coordination between
//! `WorkerPool` and `JobTracker` without requiring a real database.

use std::collections::HashSet;
use std::sync::Arc;

use gradient_core::types::ids::*;

use gradient_core::types::proto::{
    CandidateScore, FlakeJob, FlakeTask, GradientCapabilities, JobKind,
};

use super::Scheduler;
use super::jobs::{PendingBuildJob, PendingEvalJob};

/// Create a scheduler backed by a mock DB that returns empty results.
fn test_scheduler() -> Arc<Scheduler> {
    use sea_orm::{DatabaseBackend, MockDatabase};
    use test_support::prelude::*;

    let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
    let state = test_state(db);
    Arc::new(Scheduler::new(state))
}

fn eval_job(peer: OrganizationId) -> PendingEvalJob {
    PendingEvalJob {
        evaluation_id: EvaluationId::now_v7(),
        project_id: None,
        peer_id: peer,
        commit_id: CommitId::now_v7(),
        repository: "https://example.com/repo".into(),
        job: FlakeJob {
            tasks: vec![FlakeTask::EvaluateDerivations],
            source: gradient_core::types::proto::FlakeSource::Repository {
                url: "https://example.com/repo".into(),
                commit: "abc123".into(),
            },
            wildcards: vec!["*".into()],
            timeout_secs: None,
            input_overrides: vec![],
        },
        required_paths: vec![],
        queued_at: gradient_core::types::now(),
        ready_at: gradient_core::types::now(),
        rescore_count: 0,
    }
}

#[tokio::test]
async fn test_enqueue_and_get_candidates() {
    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();

    scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await;
    scheduler
        .enqueue_eval_job("j2".into(), eval_job(peer))
        .await;

    // Open mode (empty authorized peers) → see all jobs.
    let candidates = scheduler.get_job_candidates("w1").await;
    assert_eq!(candidates.len(), 2);
}

/// Regression (#359): an enqueue that happens while a session is NOT parked on
/// the notification must still be observed on its next check. The `watch`
/// generation is level-triggered, unlike the old `Notify::notify_waiters()`
/// which dropped any wakeup fired while no task was awaiting — starving deep
/// build chains of `JobOffer`s under load.
#[tokio::test]
async fn job_notify_bump_is_not_lost_when_not_awaiting() {
    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();

    let mut rx = scheduler.job_notify();
    assert!(!rx.has_changed().unwrap(), "fresh receiver starts un-bumped");

    scheduler.enqueue_eval_job("j1".into(), eval_job(peer)).await;
    assert!(rx.has_changed().unwrap(), "missed-while-busy enqueue must still be observed");

    rx.changed().await.unwrap();
    assert!(!rx.has_changed().unwrap(), "consuming the change resets the flag");

    scheduler.enqueue_eval_job("j2".into(), eval_job(peer)).await;
    assert!(rx.has_changed().unwrap(), "a later enqueue re-arms the receiver");
}

#[tokio::test]
async fn test_candidates_filtered_by_authorized_peers() {
    let scheduler = test_scheduler();
    let peer_a = OrganizationId::now_v7();
    let peer_b = OrganizationId::now_v7();

    scheduler
        .register_worker(
            "w1",
            GradientCapabilities::default(),
            HashSet::from([peer_a]),
        )
        .await;

    scheduler
        .enqueue_eval_job("ja".into(), eval_job(peer_a))
        .await;
    scheduler
        .enqueue_eval_job("jb".into(), eval_job(peer_b))
        .await;

    let candidates = scheduler.get_job_candidates("w1").await;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].job_id, "ja");
}

#[tokio::test]
async fn test_score_assignment_flow() {
    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();

    scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await;

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
    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();

    scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await;

    // Assign via RequestJob.
    scheduler.request_job("w1", JobKind::Flake).await;
    assert_eq!(scheduler.pending_job_count().await, 0);

    // Worker rejects the job → back to pending.
    scheduler.job_rejected("w1", "j1").await;
    assert_eq!(scheduler.pending_job_count().await, 1);
}

#[tokio::test]
async fn test_worker_disconnect_requeues_jobs() {
    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();

    scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await;
    scheduler
        .enqueue_eval_job("j2".into(), eval_job(peer))
        .await;

    // Assign both via RequestJob.
    scheduler.request_job("w1", JobKind::Flake).await;
    scheduler.request_job("w1", JobKind::Flake).await;

    assert_eq!(scheduler.pending_job_count().await, 0);

    // Worker disconnects → both jobs requeued.
    scheduler.unregister_worker("w1").await;
    assert_eq!(scheduler.pending_job_count().await, 2);
    assert_eq!(scheduler.worker_count().await, 0);

    // Another worker can pick them up.
    scheduler
        .register_worker("w2", GradientCapabilities::default(), HashSet::new())
        .await;
    let candidates = scheduler.get_job_candidates("w2").await;
    assert_eq!(candidates.len(), 2);
}

#[tokio::test]
async fn test_update_authorized_peers_expands_access() {
    let scheduler = test_scheduler();
    let peer_a = OrganizationId::now_v7();
    let peer_b = OrganizationId::now_v7();

    // Worker starts authorized for peer_a only.
    scheduler
        .register_worker(
            "w1",
            GradientCapabilities::default(),
            HashSet::from([peer_a]),
        )
        .await;

    scheduler
        .enqueue_eval_job("ja".into(), eval_job(peer_a))
        .await;
    scheduler
        .enqueue_eval_job("jb".into(), eval_job(peer_b))
        .await;

    assert_eq!(scheduler.get_job_candidates("w1").await.len(), 1);

    // Reauth adds peer_b.
    scheduler
        .update_authorized_peers("w1", HashSet::from([peer_a, peer_b]))
        .await;

    assert_eq!(scheduler.get_job_candidates("w1").await.len(), 2);
}

#[tokio::test]
async fn test_draining_worker_still_has_assigned_jobs() {
    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();

    scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;

    scheduler
        .enqueue_eval_job("j1".into(), eval_job(peer))
        .await;

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
    let scheduler = test_scheduler();

    let (notify, _abort_rx) = scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;

    scheduler.request_reauth("w1").await;

    // The notify should fire immediately - the dispatch loop would use this
    // to send an AuthChallenge to the worker.
    tokio::time::timeout(std::time::Duration::from_millis(50), notify.notified())
        .await
        .expect("reauth notify should fire immediately");
}

#[tokio::test]
async fn test_request_reauth_noop_for_disconnected_worker() {
    let scheduler = test_scheduler();
    // Should not panic when the worker is not connected.
    scheduler.request_reauth("nonexistent").await;
}

#[tokio::test]
async fn record_eval_message_drops_when_job_unknown() {
    // No active job → silently accepted, no DB insert attempted (MockDatabase
    // would panic on an unexpected exec; absence of panic proves no insert).
    let scheduler = test_scheduler();
    let r = scheduler
        .record_eval_message(
            "ghost-job",
            gradient_core::types::proto::EvalMessageLevel::Error,
            "build-prefetch".into(),
            "nope".into(),
        )
        .await;
    assert!(r.is_ok(), "missing active job must not be an error");
}

#[tokio::test]
async fn record_eval_message_inserts_for_active_build_job() {
    use crate::jobs::PendingBuildJob;
    use gradient_core::types::proto::{BuildJob, BuildTask};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use test_support::prelude::*;

    let eval_id = EvaluationId::now_v7();
    let peer = OrganizationId::now_v7();
    let build_id = BuildId::now_v7();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        }])
        .into_connection();
    let state = test_state(db);
    let scheduler = Arc::new(Scheduler::new(state));

    scheduler
        .enqueue_build_job(
            "jbuild".into(),
            PendingBuildJob {
                build_id,
                evaluation_id: eval_id,
                peer_id: peer,
                job: BuildJob {
                    builds: vec![BuildTask {
                        build_id: build_id.to_string(),
                        drv_path: "aaaa-hello.drv".into(),
                        external_cached: false,
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
                history: score::HistoryPrediction::default(),
                queued_at: gradient_core::types::now(),
                ready_at: gradient_core::types::now(),
                rescore_count: 0,
                pname: None,
            },
        )
        .await;
    // Move to assigned so active_job() finds it. A zero-missing score clears the
    // negative-total dispatch gate.
    scheduler
        .register_worker("w1", GradientCapabilities::default(), HashSet::new())
        .await;
    scheduler
        .record_scores(
            "w1",
            vec![CandidateScore { job_id: "jbuild".into(), missing_count: 0, missing_nar_size: 0 }],
        )
        .await;
    scheduler.request_job("w1", JobKind::Build).await;

    scheduler
        .record_eval_message(
            "jbuild",
            gradient_core::types::proto::EvalMessageLevel::Error,
            "build-prefetch".into(),
            "input prefetch failed: no nar_hash".into(),
        )
        .await
        .expect("insert should succeed");
}

#[tokio::test]
async fn fetch_only_completion_enqueues_cached_eval_followup() {
    use entity::evaluation::EvaluationStatus;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use test_support::prelude::*;

    let eval_id = EvaluationId::now_v7();
    let peer = OrganizationId::now_v7();
    let source_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-source";

    let archived_eval = entity::evaluation::Model {
        id: eval_id,
        repository: "https://example.com/repo".into(),
        commit: CommitId::nil(),
        wildcard: "*".into(),
        status: EvaluationStatus::Building,
        created_at: gradient_core::types::now(),
        updated_at: gradient_core::types::now(),
        flake_source: Some(source_path.into()),
        ..Default::default()
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        // register_worker resolves the worker's owning org from worker_registration;
        // an empty result means "no registration row" and skips the connection record.
        .append_query_results([Vec::<entity::worker_registration::Model>::new()])
        .append_query_results([vec![archived_eval]])
        .into_connection();
    let scheduler = Arc::new(Scheduler::new(test_state(db)));

    let mut fetch_job = eval_job(peer);
    fetch_job.evaluation_id = eval_id;
    fetch_job.job.tasks = vec![FlakeTask::FetchFlake];
    let job_id = format!("eval:{eval_id}");

    scheduler.enqueue_eval_job(job_id.clone(), fetch_job).await;
    scheduler
        .register_worker(
            "w1",
            GradientCapabilities { eval: true, fetch: true, ..GradientCapabilities::default() },
            HashSet::new(),
        )
        .await;
    scheduler.request_job("w1", JobKind::Flake).await;

    scheduler
        .handle_job_completed("w1", &job_id)
        .await
        .expect("fetch-only completion should succeed");

    assert_eq!(scheduler.pending_job_count().await, 1);

    // The follow-up reuses the `eval:{id}` id and carries a cached source. (We
    // inspect the pending tracker rather than dispatching, since under the new
    // negative-total gate a fetch-capable worker is reserved off cached-eval
    // work by ReserveFetchWorkersRule when spare capacity is unknown.)
    let tracker = scheduler.job_tracker.read().await;
    let follow = match tracker.pending_job(&job_id).expect("follow-up must be pending") {
        crate::jobs::PendingJob::Eval(e) => e,
        other => panic!("expected an eval follow-up, got {other:?}"),
    };
    match &follow.job.source {
        gradient_core::types::proto::FlakeSource::Cached { store_path } => {
            assert_eq!(store_path, source_path)
        }
        other => panic!("expected Cached source, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_evaluation_jobs_drops_eval_and_build_jobs() {
    use gradient_core::types::proto::{BuildJob, BuildTask};

    let scheduler = test_scheduler();
    let peer = OrganizationId::now_v7();
    let eval_id = EvaluationId::now_v7();
    let build_id_a = BuildId::now_v7();
    let build_id_b = BuildId::now_v7();

    scheduler
        .enqueue_eval_job(
            format!("eval:{eval_id}"),
            PendingEvalJob {
                evaluation_id: eval_id,
                project_id: None,
                peer_id: peer,
                commit_id: CommitId::now_v7(),
                repository: "https://example.com/repo".into(),
                job: gradient_core::types::proto::FlakeJob {
                    tasks: vec![gradient_core::types::proto::FlakeTask::EvaluateDerivations],
                    source: gradient_core::types::proto::FlakeSource::Repository {
                        url: "https://example.com/repo".into(),
                        commit: "abc123".into(),
                    },
                    wildcards: vec!["*".into()],
                    timeout_secs: None,
                    input_overrides: vec![],
                },
                required_paths: vec![],
                queued_at: gradient_core::types::now(),
                ready_at: gradient_core::types::now(),
                rescore_count: 0,
            },
        )
        .await;

    for (build_id, job_id) in [
        (build_id_a, format!("build:{build_id_a}")),
        (build_id_b, format!("build:{build_id_b}")),
    ] {
        scheduler
            .enqueue_build_job(
                job_id,
                PendingBuildJob {
                    build_id,
                    evaluation_id: eval_id,
                    peer_id: peer,
                    job: BuildJob {
                        builds: vec![BuildTask {
                            build_id: build_id.to_string(),
                            drv_path: "aaaa-hello.drv".into(),
                            external_cached: false,
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
                    history: score::HistoryPrediction::default(),
                    queued_at: gradient_core::types::now(),
                    ready_at: gradient_core::types::now(),
                    rescore_count: 0,
                    pname: None,
                },
            )
            .await;
    }

    assert_eq!(scheduler.pending_job_count().await, 3);

    scheduler
        .cancel_evaluation_jobs(eval_id, &[build_id_a, build_id_b])
        .await;

    let tracker = scheduler.job_tracker.read().await;
    assert!(!tracker.contains_job(&format!("eval:{eval_id}")));
    assert!(!tracker.contains_job(&format!("build:{build_id_a}")));
    assert!(!tracker.contains_job(&format!("build:{build_id_b}")));
    assert_eq!(tracker.pending_count() + tracker.active_count(), 0);
}
