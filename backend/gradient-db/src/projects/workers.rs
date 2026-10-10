/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::ProjectId;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub async fn project_has_eval_capable_worker_registration<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<bool, sea_orm::DbErr> {
    use gradient_entity::worker_registration::{Column as CWR, Entity as EWR};

    let row = EWR::find()
        .filter(CWR::PeerId.eq(project))
        .filter(CWR::Active.eq(true))
        .filter(CWR::EnableEval.eq(true))
        .one(db)
        .await?;
    if row.is_some() {
        return Ok(true);
    }

    Ok(crate::teams::workers::team_workers_for_project(db, project)
        .await?
        .iter()
        .any(|(_, worker)| worker.active && worker.enable_eval))
}

pub async fn worker_ids_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<String>, sea_orm::DbErr> {
    use gradient_entity::worker_registration::{Column as CWR, Entity as EWR};

    let mut workers: Vec<String> = EWR::find()
        .filter(CWR::PeerId.eq(project))
        .all(db)
        .await?
        .into_iter()
        .map(|registration| registration.worker_id)
        .collect();
    workers.extend(crate::teams::workers::team_worker_ids_for_project(db, project).await?);

    Ok(workers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::ids::{UserId, WorkerRegistrationId};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn registration_row(
        active: bool,
        enable_eval: bool,
    ) -> gradient_entity::worker_registration::Model {
        gradient_entity::worker_registration::Model {
            id: WorkerRegistrationId::now_v7(),
            peer_id: ProjectId::nil(),
            worker_id: "00000000-0000-4000-8000-000000000001".into(),
            active,
            enable_fetch: true,
            enable_eval,
            enable_build: true,
            created_by: Some(UserId::nil()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn returns_true_when_eval_capable_registration_exists() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![registration_row(true, true)]])
            .into_connection();

        let out = project_has_eval_capable_worker_registration(&db, ProjectId::nil())
            .await
            .unwrap();
        assert!(out);
    }

    #[tokio::test]
    async fn returns_false_when_no_registrations_exist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::worker_registration::Model>::new()])
            .append_query_results([Vec::<gradient_entity::team_project::Model>::new()])
            .into_connection();

        let out = project_has_eval_capable_worker_registration(&db, ProjectId::nil())
            .await
            .unwrap();
        assert!(!out);
    }

    #[tokio::test]
    async fn a_project_lists_its_registered_and_its_team_workers() {
        use gradient_entity::{team, team_project, team_worker};

        let team = gradient_types::ids::TeamId::now_v7();
        let registered = registration_row(true, true);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![registered.clone()]])
            .append_query_results([vec![team_project::Model {
                team,
                includes_workers: true,
                ..Default::default()
            }]])
            .append_query_results([vec![team::Model {
                id: team,
                ..Default::default()
            }]])
            .append_query_results([vec![team_worker::Model {
                team,
                worker_id: "team-worker".into(),
                ..Default::default()
            }]])
            .into_connection();

        let workers = worker_ids_for_project(&db, ProjectId::nil()).await.unwrap();

        assert_eq!(
            workers,
            vec![registered.worker_id, "team-worker".to_string()]
        );
    }
}
