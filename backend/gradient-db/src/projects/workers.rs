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
}
