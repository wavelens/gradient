/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Iterable};

#[derive(Debug, FromQueryResult)]
pub struct ObservedCount {
    pub kind: String,
    pub status: Option<i32>,
    pub value: i64,
}

// Status sets and label names are coming from the enums. A new or renumbered variant can never
// silently vanish from a series.
fn observations_sql() -> String {
    let build_terminal: Vec<BuildStatus> = BuildStatus::iter()
        .filter(|s| {
            s.is_terminal_success() || s.is_terminal_failure() || *s == BuildStatus::Aborted
        })
        .collect();
    format!(
        r#"
        SELECT CASE WHEN status IN ({build_terminal}) THEN 'build_total'
                    ELSE 'build_in_state' END::text AS kind,
               status::int AS status,
               COUNT(*)::bigint AS value
        FROM derivation_build
        GROUP BY status

        UNION ALL

        SELECT CASE WHEN status IN ({eval_terminal}) THEN 'evaluation_total'
                    ELSE 'evaluation_in_state' END::text,
               status::int,
               COUNT(*)::bigint
        FROM evaluation
        GROUP BY status

        UNION ALL

        SELECT 'cache_bytes'::text, NULL::int, COALESCE(SUM(file_size), 0)::bigint
        FROM cached_path

        UNION ALL

        SELECT 'cache_nar_bytes'::text, NULL::int, COALESCE(SUM(nar_size), 0)::bigint
        FROM cached_path

        UNION ALL

        SELECT 'cache_packages'::text, NULL::int, COUNT(*)::bigint
        FROM cached_path_signature

        UNION ALL

        SELECT 'cache_nar_bytes_sent_total'::text, NULL::int,
               COALESCE(SUM(bytes_sent), 0)::bigint
        FROM cache_metric

        UNION ALL

        SELECT 'cache_nar_requests_total'::text, NULL::int,
               COALESCE(SUM(nar_count)::bigint, 0)
        FROM cache_metric
    "#,
        build_terminal = crate::sql::status::build_in(&build_terminal),
        eval_terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

crate::sql_fn! {
    OBSERVATIONS = observations_sql,
        params = [],
        tier = Bulk;
}

pub async fn observed_counts<C: ConnectionTrait>(db: &C) -> Result<Vec<ObservedCount>, DbErr> {
    ObservedCount::find_by_statement(OBSERVATIONS.stmt())
        .all(db)
        .await
}
