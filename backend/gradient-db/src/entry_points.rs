/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, Order, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Select,
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

/// Failed builds come first, then builds with failed dependencies, then the rest. The first page can show what to fix.
fn failure_rank_sql() -> String {
    use gradient_entity::build::BuildStatus::*;
    format!(
        "(SELECT CASE WHEN db.status IN ({failed}) THEN 0 WHEN db.status = {dependency_failed} THEN 1 ELSE 2 END \
         FROM derivation_build db WHERE db.derivation = \"entry_point\".\"derivation\")",
        failed = crate::sql::status::build_in(&[FailedPermanent, FailedTransient, FailedTimeout]),
        dependency_failed = i32::from(DependencyFailed),
    )
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
        .order_by(Expr::cust(failure_rank_sql()), Order::Asc)
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

pub async fn any_evaluation_ever_has_entry_point_with_attribute<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    attribute: &str,
) -> Result<bool, DbErr> {
    Ok(EEntryPoint::find()
        .filter(CEntryPoint::Task.eq(task))
        .filter(CEntryPoint::Eval.eq(attribute))
        .one(db)
        .await?
        .is_some())
}

fn self_finished_for_path(task: TaskId, attribute: &str) -> Select<EEntryPoint> {
    let self_finished = Query::select()
        .column(CEvaluation::Id)
        .from(gradient_entity::evaluation::Entity)
        .and_where(CEvaluation::Task.eq(task))
        .and_where(CEvaluation::Status.is_in(EvaluationStatus::RUN_TIME_AVERAGE_STATUSES))
        .to_owned();
    EEntryPoint::find()
        .filter(CEntryPoint::Task.eq(task))
        .filter(CEntryPoint::Eval.eq(attribute))
        .filter(CEntryPoint::Evaluation.in_subquery(self_finished))
        .order_by_desc(CEntryPoint::CreatedAt)
}

pub async fn latest_self_finished_for_path<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    attribute: &str,
    limit: u64,
) -> Result<Vec<MEntryPoint>, DbErr> {
    self_finished_for_path(task, attribute)
        .limit(limit)
        .all(db)
        .await
}

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<MEntryPoint>, DbErr> {
    EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation))
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
    fn metrics_read_entry_points_of_completed_and_failed_evaluations_only() {
        let sql = self_finished_for_path(TaskId::now_v7(), "packages.hello")
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(str::contains(&sql, r#""status" IN (5, 6)"#), "{sql}");
    }

    #[test]
    fn a_blank_search_term_keeps_every_entry_point() {
        assert!(!str::contains(&entry_point_sql(Some("  ")), "ILIKE"));
        assert!(!str::contains(&entry_point_sql(None), "ILIKE"));
    }

    #[test]
    fn sql_entry_point_order_ranks_failed_builds_first_failed_dependency_second_other_builds_last()
    {
        let sql = failure_rank_sql();
        assert!(
            str::contains(
                &sql,
                "WHEN db.status IN (4, 8, 9) THEN 0 WHEN db.status = 6 THEN 1 ELSE 2"
            ),
            "{sql}"
        );
    }
}
