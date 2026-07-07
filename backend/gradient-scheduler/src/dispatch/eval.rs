/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::input::vec_to_hex;
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
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

    // One tracker snapshot instead of a read lock per eval; `add_pending` is
    // idempotent under the write lock, so the snapshot race is harmless.
    let evals: Vec<MEvaluation> = {
        let tracker = scheduler.job_tracker.read().await;
        evals
            .into_iter()
            .filter(|e| !tracker.contains_job(&format!("eval:{}", e.id)))
            .collect()
    };
    if evals.is_empty() {
        return Ok(());
    }

    let maps = EvalDispatchMaps::load(state, &evals).await?;
    let split_fetch = scheduler
        .worker_pool
        .read()
        .await
        .has_idle_eval_only_worker();
    let eval_history = scheduler.eval_history.load();

    for eval in evals {
        let job_id = format!("eval:{}", eval.id);

        let Some(commit) = maps.commits.get(&eval.commit) else {
            error!(evaluation_id = %eval.id, "commit not found for evaluation");
            continue;
        };

        // An `input_update` eval's own commit is blank (the generated flake.lock
        // commit is unknown until the PR is pushed); fetch from the base recorded
        // in the sidecar instead.
        let sidecar = maps.sidecars.get(&eval.id);
        let commit_sha = match sidecar {
            Some(s) => s.base_commit.clone(),
            None => vec_to_hex(&commit.hash),
        };

        let input_overrides = maps.overrides.get(&eval.id).cloned().unwrap_or_default();
        let input_update = sidecar.map(|s| gradient_types::proto::InputUpdateSpec {
            generator: s.generator.clone(),
            inputs: s
                .target_inputs
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        });

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

        let Some(project_id) = eval.project else {
            error!(evaluation_id = %eval.id, "evaluation has no project");
            continue;
        };
        let Some(org_id) = maps.orgs.get(&project_id).copied() else {
            error!(evaluation_id = %eval.id, %project_id, "could not determine organization for evaluation");
            continue;
        };

        let history = eval_history.get(&project_id).copied().unwrap_or_default();

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

/// Every per-eval row a dispatch pass needs, loaded in one IN-list query per
/// table instead of a round-trip per queued evaluation.
struct EvalDispatchMaps {
    commits: HashMap<CommitId, MCommit>,
    sidecars: HashMap<EvaluationId, gradient_entity::evaluation_input_update::Model>,
    overrides: HashMap<EvaluationId, Vec<gradient_types::proto::FlakeInputOverride>>,
    orgs: HashMap<ProjectId, OrganizationId>,
}

impl EvalDispatchMaps {
    async fn load(state: &Arc<ServerState>, evals: &[MEvaluation]) -> Result<Self, sea_orm::DbErr> {
        use sea_orm::QueryOrder;

        let commit_ids: Vec<CommitId> = evals
            .iter()
            .map(|e| e.commit)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let commits = gradient_db::fetch_in_chunks(&commit_ids, |chunk| async move {
            ECommit::find()
                .filter(CCommit::Id.is_in(chunk))
                .all(&state.worker_db)
                .await
        })
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();

        let update_ids: Vec<EvaluationId> = evals
            .iter()
            .filter(|e| e.kind == gradient_entity::evaluation::EvaluationKind::InputUpdate)
            .map(|e| e.id)
            .collect();
        use gradient_entity::evaluation_input_update as eiu;
        let sidecars = gradient_db::fetch_in_chunks(&update_ids, |chunk| async move {
            eiu::Entity::find()
                .filter(eiu::Column::Evaluation.is_in(chunk))
                .all(&state.worker_db)
                .await
        })
        .await?
        .into_iter()
        .map(|s| (s.evaluation, s))
        .collect();

        let eval_ids: Vec<EvaluationId> = evals.iter().map(|e| e.id).collect();
        use gradient_entity::evaluation_flake_input_override as efio;
        let mut overrides: HashMap<EvaluationId, Vec<gradient_types::proto::FlakeInputOverride>> =
            HashMap::new();
        for r in gradient_db::fetch_in_chunks(&eval_ids, |chunk| async move {
            efio::Entity::find()
                .filter(efio::Column::Evaluation.is_in(chunk))
                .order_by_asc(efio::Column::InputName)
                .all(&state.worker_db)
                .await
        })
        .await?
        {
            overrides.entry(r.evaluation).or_default().push(
                gradient_types::proto::FlakeInputOverride {
                    input_name: r.input_name,
                    url: r.url,
                },
            );
        }

        let project_ids: Vec<ProjectId> = evals
            .iter()
            .filter_map(|e| e.project)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let orgs = gradient_db::fetch_in_chunks(&project_ids, |chunk| async move {
            EProject::find()
                .filter(CProject::Id.is_in(chunk))
                .all(&state.worker_db)
                .await
        })
        .await?
        .into_iter()
        .map(|p| (p.id, p.organization))
        .collect();

        Ok(Self {
            commits,
            sidecars,
            overrides,
            orgs,
        })
    }
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
