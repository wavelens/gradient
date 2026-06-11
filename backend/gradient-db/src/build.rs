/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::Entity;
use gradient_entity::ids::BuildId;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, EntityTrait, Statement};

/// Returns the subset of `build_ids` whose every dependency has a `Completed`(3)
/// or `Substituted`(7) build in the same evaluation. Antijoin mirrors
/// `gradient-scheduler/src/dispatch.rs:680`; behavioral correctness is covered by end-to-end CI.
pub async fn builds_with_satisfied_deps<C: ConnectionTrait>(
    db: &C,
    build_ids: &[BuildId],
) -> Result<std::collections::HashSet<BuildId>, DbErr> {
    use std::collections::HashSet;
    if build_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = crate::fetch_in_chunks(build_ids, |chunk| async move {
        let placeholders = chunk
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT b.*
            FROM public.build b
            WHERE b.id IN ({placeholders})
              AND NOT EXISTS (
                  SELECT 1 FROM public.derivation_dependency dep_edge
                  WHERE dep_edge.derivation = b.derivation
                    AND NOT EXISTS (
                        SELECT 1 FROM public.build dep_build
                        WHERE dep_build.evaluation = b.evaluation
                          AND dep_build.derivation = dep_edge.dependency
                          AND dep_build.status IN (3, 7)
                    )
              )
            "#
        );
        let stmt = Statement::from_string(DbBackend::Postgres, sql);
        Entity::find().from_raw_sql(stmt).all(db).await
    })
    .await?;
    Ok(rows.into_iter().map(|b| b.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::{BuildStatus, Model as MBuild};
    use gradient_entity::ids::{DerivationId, EvaluationId};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn build_row(id: BuildId, status: BuildStatus) -> MBuild {
        MBuild {
            id,
            evaluation: EvaluationId::now_v7(),
            derivation: DerivationId::now_v7(),
            status,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn empty_input_returns_empty_set() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let result = builds_with_satisfied_deps(&db, &[]).await.unwrap();
        assert!(result.is_empty());
    }

    // MockDatabase replays preconfigured rows without executing SQL, so this only
    // verifies that returned rows are mapped to the correct BuildId set.
    #[tokio::test]
    async fn maps_returned_rows_to_id_set() {
        let id_a = BuildId::now_v7();
        let id_b = BuildId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                build_row(id_a, BuildStatus::Queued),
                build_row(id_b, BuildStatus::Queued),
            ]])
            .into_connection();

        let result = builds_with_satisfied_deps(&db, &[id_a, id_b]).await.unwrap();
        assert!(result.contains(&id_a));
        assert!(result.contains(&id_b));
    }
}
