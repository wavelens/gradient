/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::input::vec_to_hex;
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use tracing::{debug, error, info};

use crate::Scheduler;
use crate::jobs::PendingEvalJob;
use gradient_types::proto::{FlakeJob, FlakeTask, RequiredPath};

use super::DISPATCH_TICK_SECS;

pub(super) async fn eval_dispatch_loop(scheduler: Arc<Scheduler>) {
    let mut interval = tokio::time::interval(Duration::from_secs(DISPATCH_TICK_SECS));
    let cancel = scheduler.state.shutdown.token();
    info!("eval dispatch loop started");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("eval dispatch loop shutting down");
                return;
            }
            _ = interval.tick() => {}
        }
        if let Err(e) = dispatch_queued_evals(&scheduler).await {
            error!(error = %e, "eval dispatch error");
        }
    }
}

pub(crate) async fn dispatch_queued_evals(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.load(Ordering::Relaxed) {
        return Ok(());
    }

    let state = &scheduler.state;

    let evals = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Queued))
        .all(&state.worker_db)
        .await?;

    for eval in evals {
        let job_id = format!("eval:{}", eval.id);

        // Skip if already in the scheduler (pending or active).
        if scheduler.job_tracker.read().await.contains_job(&job_id) {
            continue;
        }

        let commit = match ECommit::find_by_id(eval.commit)
            .one(&state.worker_db)
            .await?
        {
            Some(c) => c,
            None => {
                error!(evaluation_id = %eval.id, "commit not found for evaluation");
                continue;
            }
        };

        let sidecar = if eval.kind == gradient_entity::evaluation::EvaluationKind::InputUpdate {
            use gradient_entity::evaluation_input_update as eiu;
            eiu::Entity::find()
                .filter(eiu::Column::Evaluation.eq(eval.id))
                .one(&state.worker_db)
                .await?
        } else {
            None
        };

        // An `input_update` eval's own commit is blank (the generated flake.lock
        // commit is unknown until the PR is pushed); fetch from the base recorded
        // in the sidecar instead.
        let commit_sha = match &sidecar {
            Some(s) => s.base_commit.clone(),
            None => vec_to_hex(&commit.hash),
        };

        let input_overrides = {
            use gradient_entity::evaluation_flake_input_override as efio;
            use sea_orm::QueryOrder;
            efio::Entity::find()
                .filter(efio::Column::Evaluation.eq(eval.id))
                .order_by_asc(efio::Column::InputName)
                .all(&state.worker_db)
                .await?
                .into_iter()
                .map(|r| gradient_types::proto::FlakeInputOverride {
                    input_name: r.input_name,
                    url: r.url,
                })
                .collect::<Vec<_>>()
        };

        let input_update = sidecar.map(|s| gradient_types::proto::InputUpdateSpec {
            generator: s.generator,
            inputs: s
                .target_inputs
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        });

        let split_fetch = scheduler.worker_pool.read().await.has_idle_eval_only_worker();
        let wildcards = eval
            .wildcard
            .parse::<Wildcard>()
            .map(|w| w.patterns().to_vec())
            .unwrap_or_else(|_| vec![eval.wildcard.clone()]);
        let (flake_job, required_paths) = flake_job_for_eval_source(
            &eval.repository,
            commit_sha,
            wildcards,
            split_fetch,
            input_overrides,
            input_update,
        );

        let organization_id = organization_id_for_eval(state, &eval).await;
        let org_id = match organization_id {
            Some(id) => id,
            None => {
                error!(evaluation_id = %eval.id, "could not determine organization for evaluation");
                continue;
            }
        };

        let history = eval
            .project
            .and_then(|p| scheduler.eval_history.load().get(&p).copied())
            .unwrap_or_default();

        let pending = PendingEvalJob {
            evaluation_id: eval.id,
            project_id: eval.project,
            org_id,
            commit_id: eval.commit,
            repository: eval.repository.clone(),
            job: flake_job,
            required_paths,
            queued_at: eval.updated_at,
            ready_at: eval.updated_at,
            rescore_count: 0,
            history,
        };

        scheduler.enqueue_eval_job(job_id.clone(), pending).await;
        debug!(evaluation_id = %eval.id, %job_id, split_fetch, "eval job enqueued");
    }

    Ok(())
}

/// Build the eval `FlakeJob` and its `required_paths` from the evaluation's
/// recorded source. A `/nix/store/...` repository is an already-materialised
/// build-request source: dispatch it as `FlakeSource::Cached` (the worker
/// substitutes the NAR and evaluates via `path:`) instead of git-cloning it.
pub(crate) fn flake_job_for_eval_source(
    repository: &str,
    commit_sha: String,
    wildcards: Vec<String>,
    split_fetch: bool,
    input_overrides: Vec<gradient_types::proto::FlakeInputOverride>,
    input_update: Option<gradient_types::proto::InputUpdateSpec>,
) -> (FlakeJob, Vec<RequiredPath>) {
    use gradient_types::proto::FlakeSource;

    if repository.starts_with("/nix/store/") {
        let job = FlakeJob {
            tasks: vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations],
            source: FlakeSource::Cached {
                store_path: repository.to_owned(),
            },
            wildcards,
            timeout_secs: None,
            input_overrides,
            input_update,
        };
        let required = vec![RequiredPath {
            path: repository.to_owned(),
            cache_info: None,
        }];

        return (job, required);
    }

    let tasks = if split_fetch {
        vec![FlakeTask::FetchFlake]
    } else {
        vec![
            FlakeTask::FetchFlake,
            FlakeTask::EvaluateFlake,
            FlakeTask::EvaluateDerivations,
        ]
    };
    let job = FlakeJob {
        tasks,
        source: FlakeSource::Repository {
            url: repository.to_owned(),
            commit: commit_sha,
        },
        wildcards,
        timeout_secs: None,
        input_overrides,
        input_update,
    };

    (job, Vec::new())
}

pub(crate) async fn organization_id_for_eval(
    state: &Arc<ServerState>,
    eval: &MEvaluation,
) -> Option<OrganizationId> {
    let project_id = eval.project.or_else(|| {
        error!(evaluation_id = %eval.id, "evaluation has no project");
        None
    })?;
    match EProject::find_by_id(project_id).one(&state.worker_db).await {
        Ok(Some(p)) => Some(p.organization),
        Ok(None) => None,
        Err(e) => {
            error!(error = %e, %project_id, "failed to load project for eval");
            None
        }
    }
}

#[cfg(test)]
mod eval_source_tests {
    use super::flake_job_for_eval_source;
    use gradient_types::proto::{FlakeSource, FlakeTask};

    #[test]
    fn cached_source_dispatches_without_fetch() {
        let (job, required) = flake_job_for_eval_source(
            "/nix/store/qgzxagd5bql1iqx0w8qzljwdlb06sn6n-source",
            "0".repeat(40),
            vec!["*".into()],
            false,
            vec![],
            None,
        );
        assert!(matches!(job.source, FlakeSource::Cached { .. }));
        assert_eq!(
            job.tasks,
            vec![FlakeTask::EvaluateFlake, FlakeTask::EvaluateDerivations]
        );
        assert!(!job.tasks.contains(&FlakeTask::FetchFlake));
        assert_eq!(required.len(), 1);
        assert_eq!(
            required[0].path,
            "/nix/store/qgzxagd5bql1iqx0w8qzljwdlb06sn6n-source"
        );
    }

    #[test]
    fn repository_source_keeps_fetch() {
        let (job, required) = flake_job_for_eval_source(
            "git@github.com:org/repo.git",
            "abc".into(),
            vec!["*".into()],
            false,
            vec![],
            None,
        );
        assert!(matches!(job.source, FlakeSource::Repository { .. }));
        assert!(job.tasks.contains(&FlakeTask::FetchFlake));
        assert!(required.is_empty());
    }
}
