/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::derivation_dependency::EdgeKind;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<MDerivationDependency>, DbErr> {
    crate::fetch_in_chunks(derivations, |chunk| {
        EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.is_in(chunk))
            .all(db)
    })
    .await
}

pub async fn build_inputs_of<C: ConnectionTrait>(
    db: &C,
    derivations: Vec<DerivationId>,
) -> Result<Vec<MDerivationDependency>, DbErr> {
    EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.is_in(derivations))
        .filter(CDerivationDependency::Kind.is_in([EdgeKind::Buildtime, EdgeKind::Both]))
        .all(db)
        .await
}

crate::sql! {
    BUILD_INPUT_PNAMES = r#"
SELECT coalesce(nullif(d.pname, ''), regexp_replace(d.name, '-[0-9][^-]*$', '')) COLLATE "C" AS pname
FROM derivation_dependency e JOIN derivation d ON d.id = e.dependency
WHERE e.derivation = $1 AND e.kind IN (0, 2)
GROUP BY 1 ORDER BY 1
"#,
        params = [DerivationId];
}

pub async fn build_input_pnames<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<String>, DbErr> {
    let rows = db
        .query_all_raw(
            BUILD_INPUT_PNAMES.bind([sea_orm::Value::Uuid(Some(derivation.into_inner()))]),
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "pname").ok())
        .collect())
}

crate::sql! {
    FEWEST_OUT_OF_MEMORY_CORES = r#"
SELECT min(build_cores) AS cores
FROM derivation_metric
WHERE derivation = $1 AND project = $2 AND oom_killed AND build_cores IS NOT NULL
"#,
        params = [DerivationId, ProjectId];
}

pub async fn fewest_out_of_memory_cores<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
    project: ProjectId,
) -> Result<Option<u32>, DbErr> {
    let row = db
        .query_one_raw(FEWEST_OUT_OF_MEMORY_CORES.bind([
            sea_orm::Value::Uuid(Some(derivation.into_inner())),
            sea_orm::Value::Uuid(Some(project.into_inner())),
        ]))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<Option<i32>>("", "cores").ok().flatten())
        .and_then(|c| u32::try_from(c).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::DerivationMetricId;
    use gradient_entity::{derivation, derivation_metric, project, user, worker_registration};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::{project_id, user_id};
    use gradient_fixtures::{rows, test_date};
    use gradient_types::ids::WorkerRegistrationId;

    #[tokio::test]
    async fn test_out_of_memory_metric_without_worker_credentials_lowers_fewest_out_of_memory_cores_for_own_project_only()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<user::Entity, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<project::Entity, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        let unrelated_project_with_own_worker_registration =
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
        let second_project_worker_registration =
            crate::rows::insert::<worker_registration::Entity, _>(
                &conn,
                worker_registration::Model {
                    id: WorkerRegistrationId::now_v7(),
                    peer_id: unrelated_project_with_own_worker_registration.id,
                    worker_id: "second-worker".into(),
                    created_by: Some(user_id()),
                    ..rows::worker_registration(true, false)
                },
            )
            .await
            .expect("the second registration is written");
        let derivation_id = DerivationId::now_v7();
        crate::rows::insert::<derivation::Entity, _>(
            &conn,
            rows::derivation_named(derivation_id, "sample"),
        )
        .await
        .expect("the derivation is written");
        let write_out_of_memory_killed_metric_row =
            async |build_cores: i32,
                   worker_registration: Option<WorkerRegistrationId>,
                   project: Option<ProjectId>| {
                crate::rows::insert::<derivation_metric::Entity, _>(
                    &conn,
                    derivation_metric::Model {
                        id: DerivationMetricId::now_v7(),
                        derivation: derivation_id,
                        architecture: "x86_64-linux".into(),
                        oom_killed: true,
                        build_cores: Some(build_cores),
                        worker_id: "worker".into(),
                        worker_registration,
                        project,
                        created_at: test_date(),
                        ..Default::default()
                    },
                )
                .await
                .expect("the metric row is written");
            };
        write_out_of_memory_killed_metric_row(
            32,
            Some(main_project_worker_registration.id),
            Some(project_id()),
        )
        .await;
        write_out_of_memory_killed_metric_row(16, None, Some(project_id())).await;
        write_out_of_memory_killed_metric_row(
            8,
            Some(second_project_worker_registration.id),
            Some(unrelated_project_with_own_worker_registration.id),
        )
        .await;
        write_out_of_memory_killed_metric_row(4, None, None).await;

        assert_eq!(
            fewest_out_of_memory_cores(&conn, derivation_id, project_id())
                .await
                .expect("the core count is read"),
            Some(16),
            "Project counts include every out-of-memory row that the server recorded for that project, with or without worker credentials. Rows from another project and old rows with no project don't count."
        );
        assert_eq!(
            fewest_out_of_memory_cores(&conn, derivation_id, ProjectId::now_v7())
                .await
                .expect("the core count is read"),
            None,
            "Projects without an out-of-memory line of their own receive no count, even when other projects contain one."
        );
        db.finish().await;
    }
}
