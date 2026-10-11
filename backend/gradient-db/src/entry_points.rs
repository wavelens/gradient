/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Select,
};

fn built(evaluation: EvaluationId, search: Option<&str>) -> Select<EEntryPoint> {
    let has_build_job = Query::select()
        .column(CBuildJob::Derivation)
        .from(gradient_entity::build_job::Entity)
        .and_where(CBuildJob::Evaluation.eq(evaluation))
        .to_owned();
    let scope = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .filter(CEntryPoint::Derivation.in_subquery(has_build_job));

    match search.map(str::trim).filter(|term| !term.is_empty()) {
        Some(term) => {
            scope.filter(Expr::col(CEntryPoint::Eval).ilike(crate::dashboard::ilike_contains(term)))
        }
        None => scope,
    }
}

pub async fn count_built<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    search: Option<&str>,
) -> Result<u64, DbErr> {
    built(evaluation, search).count(db).await
}

pub async fn built_page<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    search: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<Vec<MEntryPoint>, DbErr> {
    built(evaluation, search)
        .order_by_asc(CEntryPoint::Eval)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
}

pub async fn of_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<Vec<MEntryPoint>, DbErr> {
    EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.is_in(evaluations.iter().copied()))
        .all(db)
        .await
}

pub async fn by_attribute<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    attribute: &str,
) -> Result<Option<MEntryPoint>, DbErr> {
    EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation))
        .filter(CEntryPoint::Eval.eq(attribute))
        .one(db)
        .await
}

pub async fn latest_of_attribute<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    attribute: &str,
    limit: u64,
) -> Result<Vec<MEntryPoint>, DbErr> {
    EEntryPoint::find()
        .filter(CEntryPoint::Task.eq(task))
        .filter(CEntryPoint::Eval.eq(attribute))
        .order_by_desc(CEntryPoint::CreatedAt)
        .limit(limit)
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, QueryTrait};

    fn entry_point_sql(search: Option<&str>) -> String {
        built(EvaluationId::now_v7(), search)
            .build(DatabaseBackend::Postgres)
            .to_string()
    }

    #[test]
    fn a_search_term_narrows_entry_points_to_matching_attributes() {
        let sql = entry_point_sql(Some(" Hello "));
        assert!(str::contains(&sql, r#""eval" ILIKE '%Hello%'"#), "{sql}");
    }

    #[test]
    fn a_blank_search_term_keeps_every_entry_point() {
        assert!(!str::contains(&entry_point_sql(Some("  ")), "ILIKE"));
        assert!(!str::contains(&entry_point_sql(None), "ILIKE"));
    }
}
