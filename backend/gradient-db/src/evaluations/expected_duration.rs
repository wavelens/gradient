/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::expected_thunks::median;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};

const SAMPLED_EVALUATIONS: u64 = 10;

pub async fn get_median_duration_ms_of_last_ten_completed_evaluations_for_task_on_branch_excluding_retries<
    C: ConnectionTrait,
>(
    db: &C,
    task: TaskId,
    branch: &str,
) -> Result<Option<u64>, DbErr> {
    let recent = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task))
        .filter(CEvaluation::Branch.eq(branch))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Completed))
        .filter(CEvaluation::Kind.ne(EvaluationKind::Retry))
        .filter(CEvaluation::FinishedAt.is_not_null())
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(SAMPLED_EVALUATIONS)
        .all(db)
        .await?;
    Ok(median(recent.iter().filter_map(duration_ms).collect()))
}

fn duration_ms(evaluation: &MEvaluation) -> Option<u64> {
    let elapsed = evaluation.finished_at? - evaluation.started_at()?;
    u64::try_from(elapsed.num_milliseconds()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn the_median_skips_retries_failures_and_other_branches() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        crate::rows::insert::<ETask, _>(&conn, rows::task())
            .await
            .expect("the task is written");
        crate::rows::insert::<ECommit, _>(
            &conn,
            MCommit {
                id: gradient_fixtures::ids::commit_id(),
                ..Default::default()
            },
        )
        .await
        .expect("the commit is written");

        let runs = [
            (
                0,
                "main",
                EvaluationStatus::Completed,
                EvaluationKind::Normal,
                60,
            ),
            (
                1,
                "main",
                EvaluationStatus::Completed,
                EvaluationKind::Normal,
                120,
            ),
            (
                2,
                "main",
                EvaluationStatus::Completed,
                EvaluationKind::Normal,
                600,
            ),
            (
                3,
                "main",
                EvaluationStatus::Completed,
                EvaluationKind::Retry,
                5,
            ),
            (
                4,
                "main",
                EvaluationStatus::Failed,
                EvaluationKind::Normal,
                7,
            ),
            (
                5,
                "next",
                EvaluationStatus::Completed,
                EvaluationKind::Normal,
                9,
            ),
        ];
        for (offset, branch, status, kind, seconds) in runs {
            let evaluation = rows::eval_at(EvaluationId::now_v7(), offset);
            crate::rows::insert::<EEvaluation, _>(
                &conn,
                MEvaluation {
                    branch: branch.into(),
                    status,
                    kind,
                    fetch_started_at: Some(evaluation.created_at),
                    finished_at: Some(evaluation.created_at + chrono::Duration::seconds(seconds)),
                    ..evaluation
                },
            )
            .await
            .expect("the evaluation is written");
        }

        let main =
            get_median_duration_ms_of_last_ten_completed_evaluations_for_task_on_branch_excluding_retries(
                &conn,
                gradient_fixtures::ids::task_id(),
                "main",
            )
            .await
            .expect("the median loads");
        let empty =
            get_median_duration_ms_of_last_ten_completed_evaluations_for_task_on_branch_excluding_retries(
                &conn,
                gradient_fixtures::ids::task_id(),
                "release",
            )
            .await
            .expect("the median loads");

        assert_eq!(main, Some(120_000));
        assert_eq!(empty, None);
        db.finish().await;
    }
}
