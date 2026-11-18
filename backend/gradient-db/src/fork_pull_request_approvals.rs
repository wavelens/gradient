/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};

pub async fn add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals<
    C: ConnectionTrait,
>(
    db: &C,
    approval: MForkPullRequestApproval,
) -> Result<MForkPullRequestApproval, DbErr> {
    EForkPullRequestApproval::delete_many()
        .filter(CForkPullRequestApproval::Task.eq(approval.task))
        .filter(CForkPullRequestApproval::PrNumber.eq(approval.pr_number))
        .filter(CForkPullRequestApproval::CommitHash.ne(approval.commit_hash.clone()))
        .filter(CForkPullRequestApproval::ApprovedAt.is_null())
        .exec(db)
        .await?;

    let (task, pr_number, commit_hash) = (
        approval.task,
        approval.pr_number,
        approval.commit_hash.clone(),
    );
    EForkPullRequestApproval::insert(approval.into_active_model())
        .on_conflict(
            OnConflict::columns([
                CForkPullRequestApproval::Task,
                CForkPullRequestApproval::PrNumber,
                CForkPullRequestApproval::CommitHash,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;

    EForkPullRequestApproval::find()
        .filter(CForkPullRequestApproval::Task.eq(task))
        .filter(CForkPullRequestApproval::PrNumber.eq(pr_number))
        .filter(CForkPullRequestApproval::CommitHash.eq(commit_hash))
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("fork_pull_request_approval".to_owned()))
}

pub async fn is_commit_approved_by_maintainer_for_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    pr_number: u64,
    commit_hash: &[u8],
) -> Result<bool, DbErr> {
    Ok(EForkPullRequestApproval::find()
        .filter(CForkPullRequestApproval::Task.eq(task))
        .filter(CForkPullRequestApproval::PrNumber.eq(pr_number as i64))
        .filter(CForkPullRequestApproval::CommitHash.eq(commit_hash.to_vec()))
        .filter(CForkPullRequestApproval::ApprovedAt.is_not_null())
        .one(db)
        .await?
        .is_some())
}

pub async fn get_newest_pending_pull_request_approval_for_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    pr_number: u64,
) -> Result<Option<MForkPullRequestApproval>, DbErr> {
    EForkPullRequestApproval::find()
        .filter(CForkPullRequestApproval::Task.eq(task))
        .filter(CForkPullRequestApproval::PrNumber.eq(pr_number as i64))
        .filter(CForkPullRequestApproval::ApprovedAt.is_null())
        .order_by_desc(CForkPullRequestApproval::CreatedAt)
        .one(db)
        .await
}

pub async fn get_pending_pull_request_approval_for_task_commit<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    pr_number: u64,
    commit_hash: &[u8],
) -> Result<Option<MForkPullRequestApproval>, DbErr> {
    EForkPullRequestApproval::find()
        .filter(CForkPullRequestApproval::Task.eq(task))
        .filter(CForkPullRequestApproval::PrNumber.eq(pr_number as i64))
        .filter(CForkPullRequestApproval::CommitHash.eq(commit_hash.to_vec()))
        .filter(CForkPullRequestApproval::ApprovedAt.is_null())
        .one(db)
        .await
}

pub async fn find_approval_by_check_run_id<C: ConnectionTrait>(
    db: &C,
    check_run_id: i64,
) -> Result<Option<MForkPullRequestApproval>, DbErr> {
    EForkPullRequestApproval::find()
        .filter(CForkPullRequestApproval::CheckRunId.eq(check_run_id))
        .one(db)
        .await
}

/// Two simultaneous approvals can start only one evaluation because of the `approved_at IS NULL` filter.
pub async fn mark_pull_request_approval_approved<C: ConnectionTrait>(
    db: &C,
    approval: PullRequestApprovalId,
    maintainer_login: &str,
) -> Result<Option<MForkPullRequestApproval>, DbErr> {
    Ok(EForkPullRequestApproval::update_many()
        .col_expr(
            CForkPullRequestApproval::ApprovingMaintainerLogin,
            Expr::value(maintainer_login),
        )
        .col_expr(
            CForkPullRequestApproval::ApprovedAt,
            Expr::value(gradient_types::now()),
        )
        .filter(CForkPullRequestApproval::Id.eq(approval))
        .filter(CForkPullRequestApproval::ApprovedAt.is_null())
        .exec_with_returning(db)
        .await?
        .into_iter()
        .next())
}

pub async fn store_approval_check_run_id<C: ConnectionTrait>(
    db: &C,
    approval: PullRequestApprovalId,
    check_run_id: i64,
) -> Result<(), DbErr> {
    EForkPullRequestApproval::update_many()
        .col_expr(
            CForkPullRequestApproval::CheckRunId,
            Expr::value(check_run_id),
        )
        .filter(CForkPullRequestApproval::Id.eq(approval))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::shape::shape;
    use gradient_fixtures::test_date;
    use gradient_types::triggers::TriggerType;

    const APPROVAL_TEST_PULL_REQUEST_NUMBER: u64 = 7;

    async fn create_test_database_with_task_and_pull_request_trigger()
    -> (TestDatabase, TaskId, TaskTriggerId) {
        let (evaluation, trigger) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
        let db = shape()
            .evaluation(evaluation)
            .trigger(
                trigger,
                TriggerType::ReporterPullRequest,
                serde_json::json!({}),
                true,
                None,
            )
            .into_database()
            .await;
        let task = db.task_of(evaluation).await;
        (db, task, trigger)
    }

    fn build_pending_fork_pull_request_approval(
        task: TaskId,
        trigger: TaskTriggerId,
        commit_hash: u8,
        minutes: i64,
    ) -> MForkPullRequestApproval {
        MForkPullRequestApproval {
            id: PullRequestApprovalId::now_v7(),
            task,
            trigger,
            pr_number: APPROVAL_TEST_PULL_REQUEST_NUMBER as i64,
            pr_author: "external".into(),
            commit_hash: vec![commit_hash; 20],
            created_at: test_date() + chrono::Duration::minutes(minutes),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn storing_pending_approval_for_new_commit_removes_older_pending_approvals_keeps_approved_approvals_and_returns_existing_row_for_same_commit()
     {
        let (db, task, trigger) = create_test_database_with_task_and_pull_request_trigger().await;
        let conn = db.connection();
        let approved =
            add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
                &conn,
                build_pending_fork_pull_request_approval(task, trigger, 1, 0),
            )
            .await
            .expect("the first commit is stored");
        mark_pull_request_approval_approved(&conn, approved.id, "maintainer")
            .await
            .expect("the first commit is approved");
        let replaced =
            add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
                &conn,
                build_pending_fork_pull_request_approval(task, trigger, 2, 1),
            )
            .await
            .expect("the second commit is stored");

        let newest =
            add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
                &conn,
                build_pending_fork_pull_request_approval(task, trigger, 3, 2),
            )
            .await
            .expect("the third commit is stored");
        let again =
            add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
                &conn,
                build_pending_fork_pull_request_approval(task, trigger, 3, 3),
            )
            .await
            .expect("the third commit is stored again");

        assert_eq!(again.id, newest.id);
        let mut kept: Vec<_> = db
            .rows::<EForkPullRequestApproval>()
            .await
            .into_iter()
            .map(|row| row.id)
            .collect();
        kept.sort();
        let mut expected = vec![approved.id, newest.id];
        expected.sort();
        assert_eq!(kept, expected);
        assert!(!kept.contains(&replaced.id));
        let pending = get_newest_pending_pull_request_approval_for_task(
            &conn,
            task,
            APPROVAL_TEST_PULL_REQUEST_NUMBER,
        )
        .await
        .expect("the pending approval loads");
        assert_eq!(pending.map(|row| row.id), Some(newest.id));
        db.finish().await;
    }

    #[tokio::test]
    async fn test_approve_marks_approval_approved_second_approve_returns_none_and_find_by_check_run_id()
     {
        let (db, task, trigger) = create_test_database_with_task_and_pull_request_trigger().await;
        let conn = db.connection();
        let pending =
            add_pending_approval_for_pull_request_commit_and_remove_older_pending_approvals(
                &conn,
                build_pending_fork_pull_request_approval(task, trigger, 1, 0),
            )
            .await
            .expect("the commit is stored");
        store_approval_check_run_id(&conn, pending.id, 4242)
            .await
            .expect("the check run id is stored");
        let approved_before = is_commit_approved_by_maintainer_for_task(
            &conn,
            task,
            APPROVAL_TEST_PULL_REQUEST_NUMBER,
            &[1; 20],
        )
        .await
        .expect("the approval state loads");
        let pending_for_commit = get_pending_pull_request_approval_for_task_commit(
            &conn,
            task,
            APPROVAL_TEST_PULL_REQUEST_NUMBER,
            &[1; 20],
        )
        .await
        .expect("the pending approval of the commit loads");
        let pending_for_other_commit = get_pending_pull_request_approval_for_task_commit(
            &conn,
            task,
            APPROVAL_TEST_PULL_REQUEST_NUMBER,
            &[2; 20],
        )
        .await
        .expect("the pending approval of another commit loads");

        let first = mark_pull_request_approval_approved(&conn, pending.id, "maintainer")
            .await
            .expect("the first approval succeeds");
        let second = mark_pull_request_approval_approved(&conn, pending.id, "other")
            .await
            .expect("the second approval succeeds");

        assert!(!approved_before);
        assert_eq!(pending_for_commit.map(|row| row.id), Some(pending.id));
        assert!(pending_for_other_commit.is_none());
        assert!(
            get_pending_pull_request_approval_for_task_commit(
                &conn,
                task,
                APPROVAL_TEST_PULL_REQUEST_NUMBER,
                &[1; 20]
            )
            .await
            .expect("the pending approval of the approved commit loads")
            .is_none()
        );
        assert_eq!(
            first.and_then(|row| row.approving_maintainer_login),
            Some("maintainer".to_owned())
        );
        assert!(second.is_none());
        assert!(
            is_commit_approved_by_maintainer_for_task(
                &conn,
                task,
                APPROVAL_TEST_PULL_REQUEST_NUMBER,
                &[1; 20]
            )
            .await
            .expect("the approval state loads")
        );
        assert!(
            !is_commit_approved_by_maintainer_for_task(
                &conn,
                task,
                APPROVAL_TEST_PULL_REQUEST_NUMBER,
                &[2; 20]
            )
            .await
            .expect("the approval state loads")
        );
        let found = find_approval_by_check_run_id(&conn, 4242)
            .await
            .expect("the lookup succeeds");
        assert_eq!(found.map(|row| row.id), Some(pending.id));
        db.finish().await;
    }
}
