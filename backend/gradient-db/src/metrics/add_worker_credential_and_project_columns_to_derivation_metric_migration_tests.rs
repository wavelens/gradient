/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{
    DerivationId, DerivationMetricId, ProjectId, TeamWorkerId, WorkerRegistrationId,
};
use gradient_entity::{derivation, derivation_metric};
use gradient_fixtures::database::TestDatabase;
use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{DatabaseConnection, EntityTrait, QuerySelect};

const ADD_CREDENTIAL_AND_PROJECT_COLUMNS_TO_DERIVATION_METRIC_MIGRATION_NAME: &str =
    "m20261230_000002_add_worker_registration_team_worker_and_project_to_derivation_metric";

async fn revert_migrations_through_derivation_metric_credential_and_project_columns(
    db: &DatabaseConnection,
) {
    let migrations = Migrator::migrations();
    let steps = migrations.len()
        - migrations
            .iter()
            .position(|m| {
                m.name() == ADD_CREDENTIAL_AND_PROJECT_COLUMNS_TO_DERIVATION_METRIC_MIGRATION_NAME
            })
            .expect("The credential and project migrations appear in the migration list.");
    Migrator::down(db, Some(u32::try_from(steps).expect("step count")))
        .await
        .expect("Revert the migration for credentials and projects.");
}

async fn selecting_worker_registration_team_worker_and_project_from_derivation_metric_fails(
    db: &DatabaseConnection,
) -> bool {
    derivation_metric::Entity::find()
        .select_only()
        .expr_as(
            Expr::col(Alias::new("worker_registration")),
            "worker_registration",
        )
        .expr_as(Expr::col(Alias::new("team_worker")), "team_worker")
        .expr_as(Expr::col(Alias::new("project")), "project")
        .into_tuple::<(
            Option<WorkerRegistrationId>,
            Option<TeamWorkerId>,
            Option<ProjectId>,
        )>()
        .one(db)
        .await
        .is_err()
}

#[tokio::test]
async fn derivation_metric_credential_columns_migration_down_up_twice_keeps_existing_metric_row_with_null_worker_registration_team_worker_and_project()
 {
    let database = TestDatabase::create().await;
    let db = database.connection();
    let derivation_id = DerivationId::now_v7();
    crate::rows::insert::<derivation::Entity, _>(
        &db,
        gradient_fixtures::rows::derivation_named(derivation_id, "legacy-sample"),
    )
    .await
    .expect("derivation");
    let legacy_derivation_metric_id = DerivationMetricId::now_v7();
    crate::rows::insert::<derivation_metric::Entity, _>(
        &db,
        derivation_metric::Model {
            id: legacy_derivation_metric_id,
            derivation: derivation_id,
            architecture: "x86_64-linux".into(),
            peak_ram_mb: Some(512),
            worker_id: "worker".into(),
            created_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("The insert wrote a metric row.");

    for run in 1..=2 {
        revert_migrations_through_derivation_metric_credential_and_project_columns(&db).await;
        assert!(
            selecting_worker_registration_team_worker_and_project_from_derivation_metric_fails(&db)
                .await,
            "down {run}: credential and project columns of derivation_metric"
        );

        Migrator::up(&db, None)
            .await
            .expect("Credential and project migrations run.");
        let derivation_metric_row_after_rerunning_migration = database
            .find::<derivation_metric::Entity>(legacy_derivation_metric_id)
            .await
            .expect("derivation metric row");
        assert_eq!(
            (
                derivation_metric_row_after_rerunning_migration.worker_registration,
                derivation_metric_row_after_rerunning_migration.team_worker,
                derivation_metric_row_after_rerunning_migration.project,
                derivation_metric_row_after_rerunning_migration.peak_ram_mb,
            ),
            (None, None, None, Some(512)),
            "up {run} for credentials and project of the existing metric row"
        );
        assert_eq!(
            database
                .texts(
                    "SELECT indexname::text FROM pg_indexes \
                     WHERE tablename = 'derivation_metric' AND indexname IN \
                     ('idx-derivation_metric-worker_registration', \
                      'idx-derivation_metric-team_worker', 'idx-derivation_metric-project') \
                     ORDER BY 1"
                )
                .await,
            vec![
                "idx-derivation_metric-project".to_string(),
                "idx-derivation_metric-team_worker".to_string(),
                "idx-derivation_metric-worker_registration".to_string(),
            ],
            "up {run}: partial indexes on the credential and project columns of derivation_metric"
        );
    }
    database.finish().await;
}
