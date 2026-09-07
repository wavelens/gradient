/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Base-worker lookups: identity by worker_id, per-project enablement, and the
//! enabled-project set used to scope a connecting base worker.

use gradient_types::ids::{BaseWorkerId, ProjectBaseWorkerId, ProjectId, UserId};
use gradient_types::now;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::collections::HashSet;

/// Returns the enabled `base_worker` row for this worker_id, if any.
pub async fn enabled_base_worker_by_worker_id<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Option<gradient_entity::base_worker::Model>, sea_orm::DbErr> {
    use gradient_entity::base_worker::{Column as C2, Entity as E};

    E::find()
        .filter(C2::WorkerId.eq(worker_id))
        .filter(C2::Enabled.eq(true))
        .one(db)
        .await
}

/// True when `worker_id` belongs to a base worker, regardless of its `enabled` flag.
pub async fn worker_id_is_base<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<bool, sea_orm::DbErr> {
    use gradient_entity::base_worker::{Column, Entity};
    Ok(Entity::find()
        .filter(Column::WorkerId.eq(worker_id))
        .one(db)
        .await?
        .is_some())
}

/// Project UUIDs that have opted into the given base worker.
pub async fn projects_enabling_base_worker<C: ConnectionTrait>(
    db: &C,
    base_worker: BaseWorkerId,
) -> Result<Vec<ProjectId>, sea_orm::DbErr> {
    use gradient_entity::project_base_worker::{Column as C2, Entity as E};

    Ok(E::find()
        .filter(C2::BaseWorker.eq(base_worker))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.project)
        .collect())
}

/// True when the project has an enabled base worker with the `eval` gate on.
pub async fn project_has_eval_capable_base_worker<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<bool, sea_orm::DbErr> {
    use gradient_entity::base_worker::{Column as BWC, Entity as BW};
    use gradient_entity::project_base_worker::{Column as OBWC, Entity as OBW};

    let enabled_ids: Vec<BaseWorkerId> = OBW::find()
        .filter(OBWC::Project.eq(project))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.base_worker)
        .collect();

    if enabled_ids.is_empty() {
        return Ok(false);
    }

    let row = BW::find()
        .filter(BWC::Id.is_in(enabled_ids))
        .filter(BWC::Enabled.eq(true))
        .filter(BWC::EnableEval.eq(true))
        .one(db)
        .await?;
    Ok(row.is_some())
}

/// Links a project to every base worker flagged `auto_enable`, skipping the
/// ones it is already linked to. Called when a project is created so a
/// zero-config local worker is usable without a per-project opt-in click.
///
/// Returns the `worker_id`s newly linked. A worker that is already connected
/// only learns about the new project on re-authentication, so the caller must
/// ask the scheduler for one.
pub async fn enable_auto_base_workers_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    created_by: Option<UserId>,
) -> Result<Vec<String>, sea_orm::DbErr> {
    use gradient_entity::base_worker::{Column as BWC, Entity as BW};

    let auto: Vec<(BaseWorkerId, String)> = BW::find()
        .filter(BWC::AutoEnable.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.worker_id))
        .collect();

    link_base_workers(db, project, auto, created_by).await
}

/// Links every existing project to one base worker, skipping the ones already
/// linked. Runs once when an `auto_enable` base worker first appears.
pub async fn enable_base_worker_for_all_projects<C: ConnectionTrait>(
    db: &C,
    base_worker: BaseWorkerId,
    created_by: Option<UserId>,
) -> Result<usize, sea_orm::DbErr> {
    use gradient_entity::project::Entity as EProject;
    use gradient_entity::project_base_worker::{Column as OBWC, Entity as OBW};

    let linked: HashSet<ProjectId> = OBW::find()
        .filter(OBWC::BaseWorker.eq(base_worker))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.project)
        .collect();

    let rows: Vec<_> = EProject::find()
        .all(db)
        .await?
        .into_iter()
        .filter(|p| !linked.contains(&p.id))
        .map(|p| link_row(p.id, base_worker, created_by))
        .collect();

    insert_links(db, rows).await
}

async fn link_base_workers<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    base_workers: Vec<(BaseWorkerId, String)>,
    created_by: Option<UserId>,
) -> Result<Vec<String>, sea_orm::DbErr> {
    use gradient_entity::project_base_worker::{Column as OBWC, Entity as OBW};

    if base_workers.is_empty() {
        return Ok(Vec::new());
    }

    let linked: HashSet<BaseWorkerId> = OBW::find()
        .filter(OBWC::Project.eq(project))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.base_worker)
        .collect();

    let (rows, worker_ids): (Vec<_>, Vec<String>) = base_workers
        .into_iter()
        .filter(|(bw, _)| !linked.contains(bw))
        .map(|(bw, worker_id)| (link_row(project, bw, created_by), worker_id))
        .unzip();

    insert_links(db, rows).await?;
    Ok(worker_ids)
}

fn link_row(
    project: ProjectId,
    base_worker: BaseWorkerId,
    created_by: Option<UserId>,
) -> gradient_entity::project_base_worker::ActiveModel {
    gradient_entity::project_base_worker::Model {
        id: ProjectBaseWorkerId::now_v7(),
        project,
        base_worker,
        created_by,
        created_at: now(),
    }
    .into_active_model()
}

async fn insert_links<C: ConnectionTrait>(
    db: &C,
    rows: Vec<gradient_entity::project_base_worker::ActiveModel>,
) -> Result<usize, sea_orm::DbErr> {
    use gradient_entity::project_base_worker::Entity as OBW;

    if rows.is_empty() {
        return Ok(0);
    }

    let count = rows.len();
    OBW::insert_many(rows).exec(db).await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::project_base_worker::Model as ProjectBaseWorkerModel;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn projects_enabling_returns_empty_when_none() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<ProjectBaseWorkerModel>::new()])
            .into_connection();
        let out = projects_enabling_base_worker(&db, BaseWorkerId::nil())
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn worker_id_is_base_false_when_no_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::base_worker::Model>::new()])
            .into_connection();
        assert!(!worker_id_is_base(&db, "w-1").await.unwrap());
    }

    /// The row a Postgres `INSERT ... RETURNING` hands back. Without one the
    /// insert reports zero rows and sea-orm raises `RecordNotInserted`.
    fn returned_link(project: ProjectId, base_worker: BaseWorkerId) -> ProjectBaseWorkerModel {
        ProjectBaseWorkerModel {
            id: ProjectBaseWorkerId::now_v7(),
            project,
            base_worker,
            created_by: None,
            created_at: now(),
        }
    }

    #[tokio::test]
    async fn auto_enable_links_nothing_when_no_auto_base_workers() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::base_worker::Model>::new()])
            .into_connection();
        let linked = enable_auto_base_workers_for_project(&db, ProjectId::nil(), None)
            .await
            .unwrap();
        assert!(linked.is_empty());
        assert!(
            db.into_transaction_log()
                .iter()
                .flat_map(|t| t.statements())
                .all(|s| !s.sql.to_lowercase().contains("insert into")),
            "must not insert when nothing is flagged auto_enable"
        );
    }

    #[tokio::test]
    async fn auto_enable_skips_a_base_worker_the_project_already_has() {
        let bw = gradient_entity::base_worker::Model {
            id: BaseWorkerId::now_v7(),
            auto_enable: true,
            ..Default::default()
        };
        let project = ProjectId::now_v7();
        let existing = ProjectBaseWorkerModel {
            id: ProjectBaseWorkerId::now_v7(),
            project,
            base_worker: bw.id,
            created_by: None,
            created_at: now(),
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![bw]])
            .append_query_results([vec![existing]])
            .into_connection();

        let linked = enable_auto_base_workers_for_project(&db, project, None)
            .await
            .unwrap();
        assert!(
            linked.is_empty(),
            "already-linked worker must not be relinked"
        );
    }

    #[tokio::test]
    async fn sweep_links_only_the_projects_that_are_not_linked_yet() {
        let base = BaseWorkerId::now_v7();
        let linked_project = ProjectId::now_v7();
        let fresh_project = ProjectId::now_v7();
        let link = ProjectBaseWorkerModel {
            id: ProjectBaseWorkerId::now_v7(),
            project: linked_project,
            base_worker: base,
            created_by: None,
            created_at: now(),
        };
        let projects = vec![
            gradient_entity::project::Model {
                id: linked_project,
                ..Default::default()
            },
            gradient_entity::project::Model {
                id: fresh_project,
                ..Default::default()
            },
        ];
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![link]])
            .append_query_results([projects])
            .append_query_results([vec![returned_link(fresh_project, base)]])
            .into_connection();

        let n = enable_base_worker_for_all_projects(&db, base, None)
            .await
            .unwrap();
        assert_eq!(n, 1, "only the unlinked project gets a row");
    }

    #[tokio::test]
    async fn auto_enable_inserts_for_an_unlinked_base_worker() {
        let bw = gradient_entity::base_worker::Model {
            id: BaseWorkerId::now_v7(),
            worker_id: "local".to_string(),
            auto_enable: true,
            ..Default::default()
        };
        let project = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![bw.clone()]])
            .append_query_results([Vec::<ProjectBaseWorkerModel>::new()])
            .append_query_results([vec![returned_link(project, bw.id)]])
            .into_connection();

        let linked = enable_auto_base_workers_for_project(&db, project, None)
            .await
            .unwrap();
        assert_eq!(linked, vec!["local".to_string()]);
        assert!(
            db.into_transaction_log()
                .iter()
                .flat_map(|t| t.statements())
                .any(|s| s
                    .sql
                    .to_lowercase()
                    .contains("insert into \"project_base_worker\"")),
            "expected an INSERT INTO project_base_worker"
        );
    }
}
