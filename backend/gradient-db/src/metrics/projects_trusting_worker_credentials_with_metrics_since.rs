/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use chrono::NaiveDateTime;
use gradient_types::ids::{ProjectId, TeamWorkerId, WorkerRegistrationId};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};

use crate::store_path_content_variant_trust::project_trusts_worker_credentials_sql;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkerCredentialKey {
    ProjectRegisteredWorker(WorkerRegistrationId),
    TeamWorkerId(TeamWorkerId),
}

crate::sql_fn! {
    SELECT_DISTINCT_DERIVATION_METRIC_WORKER_CREDENTIALS_SINCE_JOINED_WITH_TRUSTING_PROJECTS = || format!(
        "SELECT credential.worker_registration, credential.team_worker, trusting.id AS project \
         FROM (SELECT worker_registration, NULL::uuid AS team_worker FROM derivation_metric \
               WHERE created_at >= $1 AND worker_registration IS NOT NULL \
               UNION \
               SELECT NULL::uuid, team_worker FROM derivation_metric \
               WHERE created_at >= $1 AND team_worker IS NOT NULL) credential \
         JOIN project trusting ON {}",
        project_trusts_worker_credentials_sql(
            "credential.worker_registration",
            "credential.team_worker",
            "trusting.id",
        ),
    ),
        params = [Now];
}

#[derive(FromQueryResult)]
struct WorkerRegistrationTeamWorkerProjectIdRow {
    worker_registration: Option<WorkerRegistrationId>,
    team_worker: Option<TeamWorkerId>,
    project: ProjectId,
}

pub async fn load_project_ids_trusting_worker_credentials_from_derivation_metrics_since(
    db: &impl ConnectionTrait,
    since: NaiveDateTime,
) -> Result<HashMap<WorkerCredentialKey, HashSet<ProjectId>>, DbErr> {
    let rows = WorkerRegistrationTeamWorkerProjectIdRow::find_by_statement(
        SELECT_DISTINCT_DERIVATION_METRIC_WORKER_CREDENTIALS_SINCE_JOINED_WITH_TRUSTING_PROJECTS
            .bind([since.into()]),
    )
    .all(db)
    .await?;

    let mut trusting: HashMap<WorkerCredentialKey, HashSet<ProjectId>> = HashMap::new();
    for row in rows {
        let credential = match (row.worker_registration, row.team_worker) {
            (Some(registration), _) => WorkerCredentialKey::ProjectRegisteredWorker(registration),
            (None, Some(team_worker)) => WorkerCredentialKey::TeamWorkerId(team_worker),
            (None, None) => continue,
        };
        trusting.entry(credential).or_default().insert(row.project);
    }
    Ok(trusting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::DerivationMetricId;
    use gradient_entity::{
        derivation, derivation_metric, project, role, team, team_worker, user, worker_registration,
    };
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::{project_id, user_id};
    use gradient_fixtures::{rows, test_date};
    use gradient_types::ids::DerivationId;

    #[tokio::test]
    async fn credential_to_trusting_projects_map_registers_own_project_team_workers_and_not_null_credentials_or_other_project()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<user::Entity, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<project::Entity, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        let second_project_with_own_worker_registration_and_user_team_grant =
            crate::rows::insert::<project::Entity, _>(
                &conn,
                rows::project_with_id(ProjectId::now_v7(), "second-project"),
            )
            .await
            .expect("the second project is written");
        let main_project_worker_registration =
            crate::rows::insert::<worker_registration::Entity, _>(
                &conn,
                worker_registration::Model {
                    id: WorkerRegistrationId::now_v7(),
                    peer_id: project_id(),
                    worker_id: "main-worker".into(),
                    created_by: Some(user_id()),
                    ..rows::worker_registration(true, false)
                },
            )
            .await
            .expect("the main registration is written");
        let worker_registration_from_second_project =
            crate::rows::insert::<worker_registration::Entity, _>(
                &conn,
                worker_registration::Model {
                    id: WorkerRegistrationId::now_v7(),
                    peer_id: second_project_with_own_worker_registration_and_user_team_grant.id,
                    worker_id: "second-worker".into(),
                    created_by: Some(user_id()),
                    ..rows::worker_registration(true, false)
                },
            )
            .await
            .expect("the second registration is written");
        let team = crate::rows::insert::<team::Entity, _>(&conn, rows::team())
            .await
            .expect("the team is written");
        let team_worker_row_of_team_granting_workers_to_main_project =
            crate::rows::insert::<team_worker::Entity, _>(
                &conn,
                team_worker::Model {
                    id: TeamWorkerId::now_v7(),
                    team: team.id,
                    worker_id: "team-worker".into(),
                    token_hash: "token".into(),
                    display_name: "Team".into(),
                    active: true,
                    created_by: Some(user_id()),
                    created_at: test_date(),
                    ..Default::default()
                },
            )
            .await
            .expect("the team worker is written");
        conn.execute_unprepared(&format!(
            "INSERT INTO team_project (id, team, project, includes_users, includes_workers, role, created_at) \
             VALUES (uuidv7(), '{team}', '{main}', false, true, NULL, now()), \
                    (uuidv7(), '{team}', '{second}', true, false, '{role}', now())",
            role = crate::rows::insert::<role::Entity, _>(&conn, rows::view_role())
                .await
                .expect("the role is written")
                .id,
            team = team.id,
            main = project_id(),
            second = second_project_with_own_worker_registration_and_user_team_grant.id,
        ))
        .await
        .expect("write the team grants");
        let derivation_id = DerivationId::now_v7();
        crate::rows::insert::<derivation::Entity, _>(
            &conn,
            rows::derivation_named(derivation_id, "sample"),
        )
        .await
        .expect("the derivation is written");
        let write_derivation_metric_row_with_optional_worker_registration_and_team_worker =
            async |worker_registration: Option<WorkerRegistrationId>,
                   team_worker: Option<TeamWorkerId>| {
                crate::rows::insert::<derivation_metric::Entity, _>(
                    &conn,
                    derivation_metric::Model {
                        id: DerivationMetricId::now_v7(),
                        derivation: derivation_id,
                        architecture: "x86_64-linux".into(),
                        peak_ram_mb: Some(512),
                        worker_id: "worker".into(),
                        worker_registration,
                        team_worker,
                        created_at: test_date(),
                        ..Default::default()
                    },
                )
                .await
                .expect("the metric row is written");
            };
        write_derivation_metric_row_with_optional_worker_registration_and_team_worker(
            Some(main_project_worker_registration.id),
            None,
        )
        .await;
        write_derivation_metric_row_with_optional_worker_registration_and_team_worker(
            Some(worker_registration_from_second_project.id),
            None,
        )
        .await;
        write_derivation_metric_row_with_optional_worker_registration_and_team_worker(
            None,
            Some(team_worker_row_of_team_granting_workers_to_main_project.id),
        )
        .await;
        write_derivation_metric_row_with_optional_worker_registration_and_team_worker(None, None)
            .await;

        let expected_trusting_project_ids_by_worker_credential = HashMap::from([
            (
                WorkerCredentialKey::ProjectRegisteredWorker(main_project_worker_registration.id),
                HashSet::from([project_id()]),
            ),
            (
                WorkerCredentialKey::ProjectRegisteredWorker(
                    worker_registration_from_second_project.id,
                ),
                HashSet::from([second_project_with_own_worker_registration_and_user_team_grant.id]),
            ),
            (
                WorkerCredentialKey::TeamWorkerId(
                    team_worker_row_of_team_granting_workers_to_main_project.id,
                ),
                HashSet::from([project_id()]),
            ),
        ]);
        assert_eq!(
            load_project_ids_trusting_worker_credentials_from_derivation_metrics_since(
                &conn,
                test_date() - chrono::Duration::days(1),
            )
            .await
            .expect("the trust map is loaded"),
            expected_trusting_project_ids_by_worker_credential,
            "Projects trust only their own worker registrations and the team workers of teams whose grant can include workers. No project can trust a metric row without credentials."
        );
        db.finish().await;
    }
}
