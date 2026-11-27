/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DbErr};

/// The open-`dispatched_job` arm is a dispatch gate, not a can-start gate.
/// A shared build already out must stay `Queued` for the report closing it.
pub async fn find_startable_shared_builds<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(FIND_STARTABLE_SHARED_BUILDS.stmt())
        .all(db)
        .await
}

pub async fn find_startable_shared_builds_among<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<gradient_types::MDerivationBuild>, DbErr> {
    use sea_orm::EntityTrait;
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    gradient_types::EDerivationBuild::find()
        .from_raw_sql(FIND_STARTABLE_SHARED_BUILDS_AMONG.bind([ids.into()]))
        .all(db)
        .await
}

fn startable_shared_builds_sql(scope: &str) -> String {
    let not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
        &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
    );

    format!(
        r#"
        SELECT db.*
        FROM derivation_build db
        WHERE db.status = {queued}
          {scope}
          AND {not_in_flight}
          AND EXISTS (
            SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation)
        ORDER BY
            (SELECT count(*)
               FROM derivation_dependency dd
              WHERE dd.derivation = db.derivation) DESC,
            db.updated_at ASC
        "#,
        queued = crate::sql::status::build(BuildStatus::Queued),
    )
}

fn find_startable_shared_builds_sql() -> String {
    startable_shared_builds_sql("")
}

fn find_startable_shared_builds_among_sql() -> String {
    startable_shared_builds_sql("AND db.derivation = ANY($1::uuid[])")
}

crate::sql_fn! {
    FIND_STARTABLE_SHARED_BUILDS = find_startable_shared_builds_sql,
        params = [],
        tier = Bulk;

    FIND_STARTABLE_SHARED_BUILDS_AMONG = find_startable_shared_builds_among_sql,
        params = [DerivationIds(64)];
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::empty_db;

    #[tokio::test]
    async fn no_moves_means_no_statement() {
        let db = empty_db();

        let shared_builds = find_startable_shared_builds_among(&db, &[])
            .await
            .expect("no-op");

        assert!(shared_builds.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn test_find_startable_shared_builds_returns_queued_job_named_unassigned_builds_in_dependency_order_and_among_narrows_to_given_build_ids()
     {
        let path = |key: &str| format!("/nix/store/{}-startable.drv", key.repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(gradient_types::EvaluationId::now_v7())
            .derivation(&path("1"))
            .build_status(BuildStatus::Queued)
            .job()
            .derivation(&path("2"))
            .build_status(BuildStatus::Queued)
            .derivation(&path("3"))
            .build_status(BuildStatus::Queued)
            .job()
            .open_assignment()
            .derivation(&path("4"))
            .job()
            .derivation(&path("5"))
            .build_status(BuildStatus::Queued)
            .job()
            .depends_on(&path("1"))
            .into_database()
            .await;
        db.execute("UPDATE derivation_build SET blocking_deps = 3, fetchable = false")
            .await;
        let mut ids = Vec::new();
        for key in ["1", "2", "3", "4", "5"] {
            ids.push(db.id_of(&path(key)).await);
        }
        let connection = db.connection();

        let all = find_startable_shared_builds(&connection).await.unwrap();
        let among = find_startable_shared_builds_among(&connection, &ids[..4])
            .await
            .unwrap();
        db.finish().await;

        assert_eq!(
            all.iter().map(|row| row.derivation).collect::<Vec<_>>(),
            [ids[4], ids[0]]
        );
        assert_eq!(
            among.iter().map(|row| row.derivation).collect::<Vec<_>>(),
            [ids[0]]
        );
    }
}
