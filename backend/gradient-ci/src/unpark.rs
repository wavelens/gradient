/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::projects::workers::project_has_eval_capable_worker_registration;
use gradient_types::ids::ProjectId;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;

use gradient_db::status::change_evaluation_status;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub async fn unpark_no_cache_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    unpark_for_project(db, project, |r| matches!(r, WaitingReason::NoCache)).await
}

pub async fn unpark_storage_full_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_max_storage_gb: u32,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    if gradient_db::caches::capacity::project_caches_all_full(db, project, instance_max_storage_gb)
        .await?
    {
        return Ok(Vec::new());
    }
    unpark_for_project(db, project, |r| {
        matches!(r, WaitingReason::CacheStorageFull)
    })
    .await
}

pub async fn unpark_storage_full_all<C: ConnectionTrait>(
    db: &C,
    instance_max_storage_gb: u32,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    let parked = EEvaluation::find()
        .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
        .all(db)
        .await?;

    let task_ids: Vec<TaskId> = parked
        .iter()
        .filter(|eval| {
            eval.waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json)
                .is_some_and(|r| matches!(r, WaitingReason::CacheStorageFull))
        })
        .filter_map(|eval| eval.task)
        .collect();

    let mut projects: Vec<ProjectId> = Vec::new();
    for task in gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
        ETask::find().filter(CTask::Id.is_in(chunk)).all(db).await
    })
    .await?
    {
        if !projects.contains(&task.project) {
            projects.push(task.project);
        }
    }

    let mut unparked = Vec::new();
    for project in projects {
        unparked
            .extend(unpark_storage_full_for_project(db, project, instance_max_storage_gb).await?);
    }
    Ok(unparked)
}

/// This unparks only the zero-workers, empty-unmet shape that `park_if_no_workers` wrote.
/// Other `Workers` parks belong to the build-dispatch repair pass. Callers are invoking this
/// after any registration touch. The guard prevents re-queue -> re-park churn when nothing changed.
pub async fn unpark_no_workers_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    if !project_has_eval_capable_worker_registration(db, project).await? {
        return Ok(Vec::new());
    }
    unpark_for_project(db, project, |r| {
        matches!(
            r,
            WaitingReason::Workers {
                unmet,
                connected_workers: 0,
                ..
            } if unmet.is_empty()
        )
    })
    .await
}

async fn unpark_for_project<C: ConnectionTrait, F: Fn(&WaitingReason) -> bool>(
    db: &C,
    project: ProjectId,
    matches_reason: F,
) -> Result<Vec<MEvaluation>, sea_orm::DbErr> {
    let task_ids: Vec<TaskId> = ETask::find()
        .filter(CTask::Project.eq(project))
        .all(db)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();

    if task_ids.is_empty() {
        return Ok(Vec::new());
    }

    let parked = gradient_db::fetch_in_chunks(&task_ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Task.is_in(chunk))
            .filter(CEvaluation::Status.eq(EvaluationStatus::Waiting))
            .all(db)
            .await
    })
    .await?;

    let candidates: Vec<MEvaluation> = parked
        .into_iter()
        .filter(|e| {
            e.waiting_reason
                .as_ref()
                .and_then(WaitingReason::from_json)
                .is_some_and(|r| matches_reason(&r))
        })
        .collect();

    let mut unparked = Vec::with_capacity(candidates.len());
    for eval in candidates {
        unparked.extend(unpark(db, eval.id).await?);
    }
    Ok(unparked)
}

async fn unpark<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Option<MEvaluation>, sea_orm::DbErr> {
    change_evaluation_status(db, evaluation, EvaluationStatus::UNPARK, |update| {
        update.col_expr(
            CEvaluation::WaitingReason,
            Expr::value(Option::<serde_json::Value>::None),
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::EvaluationKind;
    use gradient_fixtures::mock::mock_db;
    use gradient_fixtures::rows::worker_registration;
    use sea_orm::{Iterable, MockExecResult};

    fn waiting_eval(reason: WaitingReason) -> MEvaluation {
        gradient_entity::evaluation::Model {
            id: EvaluationId::now_v7(),
            task: Some(TaskId::now_v7()),
            commit: CommitId::now_v7(),
            wildcard: "*".into(),
            status: EvaluationStatus::Waiting,
            waiting_reason: Some(reason.to_json()),
            ..Default::default()
        }
    }

    fn make_task(project: ProjectId) -> gradient_entity::task::Model {
        gradient_entity::task::Model {
            id: TaskId::now_v7(),
            project,
            name: "p".into(),
            active: true,
            wildcard: "*".into(),
            created_by: gradient_types::ids::UserId::nil(),
            keep_evaluations: 10,
            concurrency: ConcurrencyPolicy::Skip,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn unpark_no_workers_requeues_zero_workers_park_and_skips_other_workers_parks() {
        let project = ProjectId::now_v7();
        let task = make_task(project);

        let stranded = {
            let mut e = waiting_eval(WaitingReason::workers(Vec::new(), 0, Vec::new()));
            e.task = Some(task.id);
            e
        };
        let capability_mismatch = {
            let mut e = waiting_eval(WaitingReason::workers(
                Vec::new(),
                1,
                vec!["aarch64-linux".into()],
            ));
            e.task = Some(task.id);
            e
        };

        let mut requeued = stranded.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;

        let mut parked = vec![stranded.clone(), capability_mismatch];
        parked.extend(EvaluationKind::iter().map(|kind| {
            let mut e = waiting_eval(WaitingReason::workers(
                vec![gradient_types::waiting_reason::UnmetRequirement {
                    architecture: "x86_64-linux".into(),
                    required_features: Vec::new(),
                    build_count: 1,
                }],
                0,
                Vec::new(),
            ));
            e.kind = kind;
            e.task = Some(task.id);
            e
        }));

        let db = mock_db()
            .append_query_results([vec![worker_registration(true, true)]])
            .append_query_results([vec![task.clone()]])
            .append_query_results([parked])
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_no_workers_for_project(&db, project).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, stranded.id);
        assert_eq!(out[0].status, EvaluationStatus::Queued);
        assert!(out[0].waiting_reason.is_none());
    }

    #[tokio::test]
    async fn unpark_no_workers_is_noop_when_no_eval_capable_registration() {
        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::team_project::Model>::new()])
            .into_connection();
        let out = unpark_no_workers_for_project(&db, ProjectId::now_v7())
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn unpark_storage_full_requeues_when_headroom_returns() {
        let project = ProjectId::now_v7();
        let task = make_task(project);

        let stranded = {
            let mut e = waiting_eval(WaitingReason::CacheStorageFull);
            e.task = Some(task.id);
            e
        };
        let mut requeued = stranded.clone();
        requeued.status = EvaluationStatus::Queued;
        requeued.waiting_reason = None;

        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
            .append_query_results([vec![task.clone()]])
            .append_query_results([vec![stranded.clone()]])
            .append_query_results([vec![requeued.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let out = unpark_storage_full_for_project(&db, project, 0)
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, stranded.id);
        assert_eq!(out[0].status, EvaluationStatus::Queued);
        assert!(out[0].waiting_reason.is_none());
    }
}
