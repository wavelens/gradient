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
use std::collections::HashMap;

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

pub async fn attribute_paths_by_derivation_id<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> Result<HashMap<DerivationId, Vec<String>>, DbErr> {
    let entry_points = crate::fetch_in_chunks(derivations, |chunk| {
        EEntryPoint::find()
            .filter(CEntryPoint::Evaluation.eq(evaluation))
            .filter(CEntryPoint::Derivation.is_in(chunk))
            .order_by_asc(CEntryPoint::Eval)
            .all(db)
    })
    .await?;
    let mut paths: HashMap<DerivationId, Vec<String>> = HashMap::new();
    for entry_point in entry_points {
        paths
            .entry(entry_point.derivation)
            .or_default()
            .push(entry_point.eval);
    }
    Ok(paths)
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

pub async fn newest_entry_points_for_task_attribute_path<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    attribute: &str,
    limit: u64,
) -> Result<Vec<MEntryPoint>, DbErr> {
    let completed_or_failed = Query::select()
        .column(CEvaluation::Id)
        .from(gradient_entity::evaluation::Entity)
        .and_where(CEvaluation::Task.eq(task))
        .and_where(
            CEvaluation::Status.is_in(EvaluationStatus::AVERAGE_RUN_TIME_EVALUATION_STATUSES),
        )
        .to_owned();
    EEntryPoint::find()
        .filter(CEntryPoint::Task.eq(task))
        .filter(CEntryPoint::Eval.eq(attribute))
        .filter(CEntryPoint::Evaluation.in_subquery(completed_or_failed))
        .order_by_desc(CEntryPoint::CreatedAt)
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

    #[tokio::test]
    async fn test_built_entry_points_page_and_count_with_search_ranking_and_paging() {
        use gradient_entity::build::BuildStatus;
        use gradient_fixtures::shape::shape;

        let evaluation = EvaluationId::now_v7();
        let drv = |letter: &str| format!("{}-{letter}.drv", letter.repeat(32));
        let db = shape()
            .evaluation(evaluation)
            .derivation(&drv("a"))
            .build_status(BuildStatus::Completed)
            .job()
            .entry_point("pkg.alpha")
            .derivation(&drv("b"))
            .build_status(BuildStatus::FailedPermanent)
            .job()
            .entry_point("pkg.zeta")
            .derivation(&drv("c"))
            .build_status(BuildStatus::DependencyFailed)
            .job()
            .entry_point("pkg.mid")
            .derivation(&drv("d"))
            .entry_point("pkg.hidden")
            .into_database()
            .await;
        let conn = db.connection();
        let attrs = |entry_points: Vec<MEntryPoint>| -> Vec<String> {
            entry_points.into_iter().map(|e| e.eval).collect()
        };

        let everything = built_page(&conn, evaluation, None, 0, 10).await.unwrap();
        let blank = count_built(&conn, evaluation, Some("  ")).await.unwrap();
        let searched = built_page(&conn, evaluation, Some(" ALPHA "), 0, 10)
            .await
            .unwrap();
        let searched_count = count_built(&conn, evaluation, Some(" ALPHA "))
            .await
            .unwrap();
        let second = built_page(&conn, evaluation, None, 1, 1).await.unwrap();
        let unknown = count_built(&conn, EvaluationId::now_v7(), None)
            .await
            .unwrap();
        db.finish().await;

        assert_eq!(attrs(everything), ["pkg.zeta", "pkg.mid", "pkg.alpha"]);
        assert_eq!(blank, 3, "an entry point without a build job is not built");
        assert_eq!(attrs(searched), ["pkg.alpha"]);
        assert_eq!(searched_count, 1);
        assert_eq!(attrs(second), ["pkg.mid"]);
        assert_eq!(unknown, 0);
    }

    #[tokio::test]
    async fn metrics_read_entry_points_of_completed_and_failed_evaluations_only() {
        const HELLO: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello.drv";
        let [completed, failed, aborted] = [(); 3].map(|_| EvaluationId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(completed)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(HELLO)
            .entry_point("packages.hello")
            .evaluation(failed)
            .same_task_as(completed)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(HELLO)
            .entry_point("packages.hello")
            .evaluation(aborted)
            .same_task_as(completed)
            .evaluation_status(EvaluationStatus::Aborted)
            .derivation(HELLO)
            .entry_point("packages.hello")
            .into_database()
            .await;
        let task = db.rows::<EEntryPoint>().await[0].task;

        let mut read: Vec<EvaluationId> = newest_entry_points_for_task_attribute_path(
            &db.connection(),
            task,
            "packages.hello",
            10,
        )
        .await
        .expect("the entry points are read")
        .into_iter()
        .map(|entry_point| entry_point.evaluation)
        .collect();
        read.sort();

        let mut expected = vec![completed, failed];
        expected.sort();
        assert_eq!(read, expected);
        db.finish().await;
    }
}
