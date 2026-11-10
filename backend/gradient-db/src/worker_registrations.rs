/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{dispatched_job, worker_connection, worker_sample};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};

pub async fn by_worker_id<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Option<MWorkerRegistration>, DbErr> {
    EWorkerRegistration::find()
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .one(db)
        .await
}

pub async fn of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MWorkerRegistration>, DbErr> {
    EWorkerRegistration::find()
        .filter(CWorkerRegistration::PeerId.eq(project))
        .all(db)
        .await
}

pub async fn by_project_and_worker_id<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    worker_id: &str,
) -> Result<Option<MWorkerRegistration>, DbErr> {
    EWorkerRegistration::find()
        .filter(CWorkerRegistration::PeerId.eq(project))
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .one(db)
        .await
}

pub async fn is_gradient_ci_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<bool, DbErr> {
    Ok(EWorkerRegistration::find()
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .filter(CWorkerRegistration::GradientCi.eq(true))
        .one(db)
        .await?
        .is_some())
}

pub async fn delete_by_project_and_worker_id<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    worker_id: &str,
) -> Result<u64, DbErr> {
    Ok(EWorkerRegistration::delete_many()
        .filter(CWorkerRegistration::PeerId.eq(project))
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .exec(db)
        .await?
        .rows_affected)
}

pub async fn latest_resource_samples_by_worker_id<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    limit: u64,
) -> Result<Vec<worker_sample::Model>, DbErr> {
    let mut samples = worker_sample::Entity::find()
        .filter(worker_sample::Column::WorkerId.eq(worker_id))
        .order_by_desc(worker_sample::Column::At)
        .limit(limit)
        .all(db)
        .await?;
    samples.reverse();
    Ok(samples)
}

pub async fn latest_connections<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    limit: u64,
) -> Result<Vec<worker_connection::Model>, DbErr> {
    worker_connection::Entity::find()
        .filter(worker_connection::Column::WorkerId.eq(worker_id))
        .order_by_desc(worker_connection::Column::ConnectedAt)
        .limit(limit)
        .all(db)
        .await
}

pub async fn dispatched_job_count<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    project: ProjectId,
) -> Result<u64, DbErr> {
    dispatched_job::Entity::find()
        .filter(dispatched_job::Column::WorkerId.eq(worker_id))
        .filter(dispatched_job::Column::Project.eq(project))
        .count(db)
        .await
}

pub async fn by_worker_id_or_gradient_ci_project<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    project: Option<ProjectId>,
) -> Result<Option<MWorkerRegistration>, DbErr> {
    let same_worker = Condition::any().add(CWorkerRegistration::WorkerId.eq(worker_id));
    let registrations = match project {
        None => same_worker,
        Some(project) => same_worker.add(
            Condition::all()
                .add(CWorkerRegistration::PeerId.eq(project))
                .add(CWorkerRegistration::GradientCi.eq(true)),
        ),
    };

    EWorkerRegistration::find()
        .filter(registrations)
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    registration: MWorkerRegistration,
) -> Result<MWorkerRegistration, DbErr> {
    registration.into_active_model().insert(db).await
}

pub async fn open_dispatched_jobs<C: ConnectionTrait>(
    db: &C,
    projects: Option<&[ProjectId]>,
    limit: u64,
) -> Result<Vec<dispatched_job::Model>, DbErr> {
    dispatched_job::Entity::find()
        .filter(dispatched_job::Column::FinishedAt.is_null())
        .apply_if(projects, |query, projects| {
            query.filter(dispatched_job::Column::Project.is_in(projects.iter().copied()))
        })
        .order_by_desc(dispatched_job::Column::DispatchedAt)
        .limit(limit)
        .all(db)
        .await
}

pub async fn count_running_dispatched_jobs_not_in_project_ids<C: ConnectionTrait>(
    db: &C,
    projects: &[ProjectId],
) -> Result<u64, DbErr> {
    dispatched_job::Entity::find()
        .filter(dispatched_job::Column::FinishedAt.is_null())
        .filter(dispatched_job::Column::Project.is_not_in(projects.iter().copied()))
        .count(db)
        .await
}

pub async fn phases_of_dispatched_job<C: ConnectionTrait>(
    db: &C,
    job: gradient_entity::ids::DispatchedJobId,
) -> Result<Vec<gradient_entity::dispatched_job_phase::Model>, DbErr> {
    use gradient_entity::dispatched_job_phase::{Column, Entity};

    Entity::find()
        .filter(Column::DispatchedJob.eq(job))
        .order_by_asc(Column::Seq)
        .all(db)
        .await
}

fn scoring_summary_sql(window_hours: i64, limit: u64) -> String {
    format!(
        "SELECT score, score_breakdown FROM dispatched_job \
         WHERE dispatched_at >= (now() AT TIME ZONE 'UTC') - interval '{window_hours} hours' \
         ORDER BY dispatched_at DESC LIMIT {limit}"
    )
}

crate::sql_fn! {
    SCORING_SUMMARY = || scoring_summary_sql(24, 2000),
        params = [],
        tier = Bulk;
}

pub struct DispatchScore {
    pub score: f64,
    pub score_breakdown: Option<serde_json::Value>,
}

pub async fn recent_scores<C: ConnectionTrait>(
    db: &C,
    window_hours: i64,
    limit: u64,
) -> Result<Vec<DispatchScore>, DbErr> {
    let rows = db
        .query_all_raw(SCORING_SUMMARY.bind_built(scoring_summary_sql(window_hours, limit), []))
        .await?;

    Ok(rows
        .iter()
        .map(|r| DispatchScore {
            score: r.try_get("", "score").unwrap_or(0.0),
            score_breakdown: r.try_get("", "score_breakdown").ok(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn deleting_a_project_deletes_its_worker_registrations() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let project = rows::project();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<EProject, _>(&conn, project.clone())
            .await
            .expect("the project is written");
        crate::rows::insert::<EWorkerRegistration, _>(
            &conn,
            MWorkerRegistration {
                peer_id: project.id,
                created_by: None,
                ..rows::worker_registration(true, true)
            },
        )
        .await
        .expect("the registration is written");

        crate::rows::delete::<EProject, _>(&conn, project)
            .await
            .expect("the project is deleted");

        assert!(db.rows::<EWorkerRegistration>().await.is_empty());
        db.finish().await;
    }

    fn running_job(project: ProjectId) -> dispatched_job::Model {
        dispatched_job::Model {
            id: gradient_entity::ids::DispatchedJobId::now_v7(),
            project,
            worker_id: "w1".into(),
            score_breakdown: serde_json::json!({}),
            worker_context: serde_json::json!({}),
            job_context: serde_json::json!({}),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn running_jobs_outside_the_visible_projects_are_counted_but_not_listed() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let (visible, hidden) = (ProjectId::now_v7(), ProjectId::now_v7());
        for project in [visible, hidden, hidden] {
            crate::rows::insert::<dispatched_job::Entity, _>(&conn, running_job(project))
                .await
                .expect("the job is written");
        }

        let listed = open_dispatched_jobs(&conn, Some(&[visible]), 500)
            .await
            .expect("the visible jobs are read");
        let hidden_count = count_running_dispatched_jobs_not_in_project_ids(&conn, &[visible])
            .await
            .expect("the hidden jobs are counted");

        assert_eq!(
            listed.iter().map(|job| job.project).collect::<Vec<_>>(),
            vec![visible]
        );
        assert_eq!(hidden_count, 2);
        db.finish().await;
    }

    #[tokio::test]
    async fn latest_resource_samples_are_the_newest_in_time_order() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let start = gradient_types::now();
        for minutes in [0, 2, 1, 3] {
            crate::rows::insert::<worker_sample::Entity, _>(
                &conn,
                worker_sample::Model {
                    id: WorkerSampleId::now_v7(),
                    worker_id: "worker".into(),
                    at: start + chrono::Duration::minutes(minutes),
                    capabilities: serde_json::json!({}),
                    ..Default::default()
                },
            )
            .await
            .expect("the sample is written");
        }

        let samples = latest_resource_samples_by_worker_id(&conn, "worker", 2)
            .await
            .expect("the samples are read");

        let offsets: Vec<i64> = samples
            .iter()
            .map(|sample| (sample.at - start).num_minutes())
            .collect();
        assert_eq!(offsets, [2, 3]);
        db.finish().await;
    }
}
