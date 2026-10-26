/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::lookup::{by_id, by_ids};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gradient_db::WorkerDb;
use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, DispatchedJobKind, Entity as EDispatchedJob,
};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::input::vec_to_hex;
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use gradient_util::supervision::{Pass, PassError, RunsOn};
use sea_orm::sea_query::{Expr, ExprTrait, Query, SelectStatement};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use tokio::sync::Notify;
use tracing::{debug, error, warn};

use crate::Scheduler;
use crate::cluster::{Membership, Route};
use crate::instance::EvalHistory;
use crate::jobs::{PendingEvalJob, PendingJob};
use gradient_wire::types::{FlakeJob, FlakeStep, RequiredPath};

pub(crate) struct EvalDispatch {
    pub(crate) scheduler: Arc<Scheduler>,
}

impl Pass for EvalDispatch {
    const NAME: &'static str = "eval-dispatch";
    const RUNS_ON: RunsOn = RunsOn::EveryInstance;
    const BUDGET: Duration = super::ASSIGN_BUDGET;

    fn period(&self) -> Duration {
        super::ASSIGN_TICK
    }

    fn wake(&self) -> Option<Arc<Notify>> {
        Some(Arc::clone(&self.scheduler.db.wake.eval_assign))
    }

    async fn run(&self) -> Result<(), PassError> {
        assign_queued_evals(&self.scheduler)
            .await
            .map_err(Into::into)
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn assign_queued_evals(scheduler: &Scheduler) -> anyhow::Result<()> {
    if scheduler.draining.is_draining().await {
        return Ok(());
    }

    // The open `dispatched_job` row is the durable proof a job is out. The tracker is only the
    // in-memory fast path and is empty after a respawn.
    let evals = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Queued))
        .filter(Expr::exists(open_eval_dispatch()).not())
        .all(&scheduler.db.worker_db)
        .await?;
    if evals.is_empty() {
        return Ok(());
    }

    let membership = membership_after_splits(scheduler, &evals).await?;
    let routes: HashMap<EvaluationId, Vec<(u16, String, Route)>> = evals
        .iter()
        .map(|e| (e.id, membership.eval_routes(e.id)))
        .collect();
    let keys: Vec<String> = routes
        .values()
        .flatten()
        .map(|(_, key, _)| key.clone())
        .collect();
    let untracked: HashSet<String> = scheduler.untracked(keys).await.into_iter().collect();
    let mut routes = routes;
    for routes in routes.values_mut() {
        routes.retain(|(index, key, route)| {
            untracked.contains(key) && (*index == 0 || !matches!(route, Route::Single))
        });
    }

    let evals: Vec<MEvaluation> = evals
        .into_iter()
        .filter(|e| routes.get(&e.id).is_some_and(|r| !r.is_empty()))
        .collect();
    if evals.is_empty() {
        return Ok(());
    }

    let maps = EvalAssignMaps::load(&scheduler.db.worker_db, &evals).await?;
    let split_fetch = scheduler.has_idle_eval_only_worker().await;
    let eval_history = scheduler.scoring.eval_history.load();

    for eval in evals {
        let Some(members) = routes.remove(&eval.id) else {
            continue;
        };
        let single = matches!(members.as_slice(), [(0, _, Route::Single)]);
        let split_fetch = split_fetch && single;
        let Some(pending) = assemble_eval_job(&eval, &maps, split_fetch, &eval_history) else {
            continue;
        };

        for (eval_index, job_id, route) in members {
            let pending = PendingEvalJob {
                eval_index,
                ..pending.clone()
            };
            if let Err(e) = scheduler
                .enqueue_routed(route, job_id.clone(), PendingJob::Eval(pending))
                .await
            {
                error!(error = %e, %job_id, "enqueue_eval_job failed");
                continue;
            }

            debug!(evaluation_id = %eval.id, %job_id, split_fetch, "eval job enqueued");
        }
    }

    Ok(())
}

fn open_eval_dispatch() -> SelectStatement {
    Query::select()
        .expr(Expr::val(1))
        .from(EDispatchedJob)
        .and_where(
            Expr::col((EDispatchedJob, CDispatchedJob::EvaluationId))
                .equals((gradient_entity::evaluation::Entity, CEvaluation::Id)),
        )
        .and_where(Expr::col((EDispatchedJob, CDispatchedJob::Kind)).eq(DispatchedJobKind::Eval))
        .and_where(Expr::col((EDispatchedJob, CDispatchedJob::FinishedAt)).is_null())
        .to_owned()
}

async fn membership_after_splits(
    scheduler: &Scheduler,
    evals: &[MEvaluation],
) -> anyhow::Result<Membership> {
    let db = &scheduler.db.worker_db;
    let ids: Vec<EvaluationId> = evals.iter().map(|e| e.id).collect();
    let membership = Membership::load(db, &ids, &[]).await?;
    scheduler
        .split_decisions
        .keep_only(&ids.iter().copied().collect());
    let mut created = false;
    for eval in evals.iter().filter(|e| !membership.has_cluster(e.id)) {
        match scheduler.decide_split(eval).await {
            Ok(split) => created |= split.is_some(),
            Err(e) => warn!(evaluation_id = %eval.id, error = %e, "split decision failed"),
        }
    }

    if !created {
        return Ok(membership);
    }

    Ok(Membership::load(db, &ids, &[]).await?)
}

fn assemble_eval_job(
    eval: &MEvaluation,
    maps: &EvalAssignMaps,
    split_fetch: bool,
    eval_history: &EvalHistory,
) -> Option<PendingEvalJob> {
    let Some(commit) = maps.commits.get(&eval.commit) else {
        error!(evaluation_id = %eval.id, "commit not found for evaluation");
        return None;
    };

    // An `input_update` evaluation's own commit is blank until the PR is pushed. The fetch is
    // using the base recorded in the sidecar instead.
    let sidecar = maps.sidecars.get(&eval.id);
    let commit_sha = match sidecar {
        Some(s) => s.base_commit.clone(),
        None => vec_to_hex(&commit.hash),
    };

    let input_overrides = maps.overrides.get(&eval.id).cloned().unwrap_or_default();
    let input_update = sidecar.map(|s| gradient_wire::types::InputUpdateSpec {
        generator: s.generator.clone(),
        discover_only: s.discover_only,
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

    let Some(task_id) = eval.task else {
        error!(evaluation_id = %eval.id, "evaluation has no task");
        return None;
    };
    let Some(project_id) = maps.projects.get(&task_id).copied() else {
        error!(evaluation_id = %eval.id, %task_id, "could not determine project for evaluation");
        return None;
    };

    let estimate = eval_history.for_job(task_id, &flake_job.steps);

    Some(PendingEvalJob {
        evaluation_id: eval.id,
        task_id: eval.task,
        project_id,
        commit_id: eval.commit,
        repository: eval.repository.clone(),
        job: flake_job,
        required_paths,
        queued_at: eval.updated_at,
        ready_at: eval.updated_at,
        rescore_count: 0,
        prioritized: eval.prioritized,
        build_request: maps.build_request_tasks.contains(&task_id),
        estimate,
        walk_mode: eval.walk_mode,
        eval_index: 0,
    })
}

pub(crate) async fn pending_eval_jobs(
    scheduler: &Scheduler,
    evaluations: Vec<MEvaluation>,
) -> anyhow::Result<Vec<(String, PendingEvalJob)>> {
    if evaluations.is_empty() {
        return Ok(Vec::new());
    }

    let maps = EvalAssignMaps::load(&scheduler.db.worker_db, &evaluations).await?;
    let eval_history = scheduler.scoring.eval_history.load();
    Ok(evaluations
        .iter()
        .filter_map(|eval| {
            let pending = assemble_eval_job(eval, &maps, false, &eval_history)?;
            Some((crate::jobs::eval_job_key(eval.id, 0), pending))
        })
        .collect())
}

struct EvalAssignMaps {
    commits: HashMap<CommitId, MCommit>,
    sidecars: HashMap<EvaluationId, gradient_entity::evaluation_input_update::Model>,
    overrides: HashMap<EvaluationId, Vec<gradient_wire::types::FlakeInputOverride>>,
    projects: HashMap<TaskId, ProjectId>,
    build_request_tasks: HashSet<TaskId>,
}

impl EvalAssignMaps {
    async fn load(db: &WorkerDb, evals: &[MEvaluation]) -> Result<Self, sea_orm::DbErr> {
        use sea_orm::QueryOrder;

        let commit_ids: Vec<CommitId> = evals
            .iter()
            .map(|e| e.commit)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let commits = by_ids::<ECommit, _>(db, &commit_ids).await?;

        let update_ids: Vec<EvaluationId> = evals
            .iter()
            .filter(|e| e.kind == gradient_entity::evaluation::EvaluationKind::InputUpdate)
            .map(|e| e.id)
            .collect();
        use gradient_entity::evaluation_input_update as eiu;
        let sidecars = gradient_db::fetch_in_chunks(&update_ids, |chunk| async move {
            eiu::Entity::find()
                .filter(eiu::Column::Evaluation.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|s| (s.evaluation, s))
        .collect();

        let eval_ids: Vec<EvaluationId> = evals.iter().map(|e| e.id).collect();
        use gradient_entity::evaluation_flake_input_override as efio;
        let mut overrides: HashMap<EvaluationId, Vec<gradient_wire::types::FlakeInputOverride>> =
            HashMap::new();
        for r in gradient_db::fetch_in_chunks(&eval_ids, |chunk| async move {
            efio::Entity::find()
                .filter(efio::Column::Evaluation.is_in(chunk))
                .order_by_asc(efio::Column::InputName)
                .all(db)
                .await
        })
        .await?
        {
            overrides.entry(r.evaluation).or_default().push(
                gradient_wire::types::FlakeInputOverride {
                    input_name: r.input_name,
                    url: r.url,
                },
            );
        }

        let task_ids: Vec<TaskId> = evals
            .iter()
            .filter_map(|e| e.task)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let tasks = gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
            ETask::find().filter(CTask::Id.is_in(chunk)).all(db).await
        })
        .await?;
        let projects = tasks.iter().map(|t| (t.id, t.project)).collect();
        let build_request_tasks = tasks
            .iter()
            .filter(|t| gradient_db::build_request_task::is_build_request_task(t))
            .map(|t| t.id)
            .collect();

        Ok(Self {
            commits,
            sidecars,
            overrides,
            projects,
            build_request_tasks,
        })
    }
}

pub(crate) fn flake_job_for_eval_source(
    repository: &str,
    commit_sha: String,
    wildcards: Vec<String>,
    split_fetch: bool,
    input_overrides: Vec<gradient_wire::types::FlakeInputOverride>,
    input_update: Option<gradient_wire::types::InputUpdateSpec>,
) -> (FlakeJob, Vec<RequiredPath>) {
    use gradient_wire::types::FlakeSource;

    if repository.starts_with("/nix/store/") {
        let steps = if split_fetch {
            vec![FlakeStep::FetchFlake]
        } else {
            vec![
                FlakeStep::FetchFlake,
                FlakeStep::EvaluateFlake,
                FlakeStep::EvaluateDerivations,
            ]
        };
        let job = FlakeJob {
            steps,
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

    let steps = if split_fetch {
        vec![FlakeStep::FetchFlake]
    } else {
        vec![
            FlakeStep::FetchFlake,
            FlakeStep::EvaluateFlake,
            FlakeStep::EvaluateDerivations,
        ]
    };
    let job = FlakeJob {
        steps,
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

pub(crate) async fn project_id_for_eval(db: &WorkerDb, eval: &MEvaluation) -> Option<ProjectId> {
    let task_id = eval.task.or_else(|| {
        error!(evaluation_id = %eval.id, "evaluation has no task");
        None
    })?;
    match by_id::<ETask, _>(db, task_id).await {
        Ok(Some(p)) => Some(p.project),
        Ok(None) => None,
        Err(e) => {
            error!(error = %e, %task_id, "failed to load task for eval");
            None
        }
    }
}

#[cfg(test)]
mod eval_source_tests {
    use super::flake_job_for_eval_source;
    use gradient_wire::types::{FlakeSource, FlakeStep};

    #[test]
    fn cached_source_assigns_with_fetch() {
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
            job.steps,
            vec![
                FlakeStep::FetchFlake,
                FlakeStep::EvaluateFlake,
                FlakeStep::EvaluateDerivations
            ]
        );
        assert_eq!(required.len(), 1);
        assert_eq!(
            required[0].path,
            "/nix/store/qgzxagd5bql1iqx0w8qzljwdlb06sn6n-source"
        );
    }

    #[test]
    fn cached_source_split_fetch_assigns_fetch_only() {
        let (job, required) = flake_job_for_eval_source(
            "/nix/store/qgzxagd5bql1iqx0w8qzljwdlb06sn6n-source",
            "0".repeat(40),
            vec!["*".into()],
            true,
            vec![],
            None,
        );
        assert!(matches!(job.source, FlakeSource::Cached { .. }));
        assert_eq!(job.steps, vec![FlakeStep::FetchFlake]);
        assert_eq!(required.len(), 1);
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
        assert!(job.steps.contains(&FlakeStep::FetchFlake));
        assert!(required.is_empty());
    }
}
