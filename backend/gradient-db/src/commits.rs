/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};

pub async fn with_hash<C: ConnectionTrait>(db: &C, hash: Vec<u8>) -> Result<Vec<MCommit>, DbErr> {
    ECommit::find().filter(CCommit::Hash.eq(hash)).all(db).await
}

pub async fn insert<C: ConnectionTrait>(db: &C, commit: MCommit) -> Result<MCommit, DbErr> {
    commit.into_active_model().insert(db).await
}

fn keep_stored_when_empty(column: &str) -> Expr {
    Expr::cust(format!(
        "CASE WHEN \"excluded\".\"{column}\" <> '' THEN \"excluded\".\"{column}\" ELSE \"commit\".\"{column}\" END"
    ))
}

/// Synthetic commits without a project, such as SSH builds, keep a row of their own.
pub async fn insert_or_update_commit_row_keep_existing_message_if_new_empty<C: ConnectionTrait>(
    db: &C,
    commit: MCommit,
) -> Result<MCommit, DbErr> {
    if commit.project.is_none() || commit.hash.iter().all(|byte| *byte == 0) {
        return insert(db, commit).await;
    }

    let fetched_at = commit.fetched_at;
    let mut on_conflict = OnConflict::columns([CCommit::Project, CCommit::Hash]);
    on_conflict
        .value(CCommit::Message, keep_stored_when_empty("message"))
        .value(CCommit::AuthorName, keep_stored_when_empty("author_name"))
        .value(
            CCommit::Author,
            Expr::cust("coalesce(\"excluded\".\"author\", \"commit\".\"author\")"),
        );
    if fetched_at.is_some() {
        on_conflict.value(
            CCommit::FetchedAt,
            Expr::cust("coalesce(\"commit\".\"fetched_at\", \"excluded\".\"fetched_at\")"),
        );
    }

    ECommit::insert(commit.into_active_model())
        .on_conflict(on_conflict.to_owned())
        .exec_with_returning(db)
        .await
}

pub async fn fill_message_from_git<C: ConnectionTrait>(
    db: &C,
    commit: CommitId,
    message: String,
    author_name: String,
) -> Result<(), DbErr> {
    ECommit::update_many()
        .col_expr(CCommit::Message, Expr::value(message))
        .col_expr(CCommit::AuthorName, Expr::value(author_name))
        .col_expr(CCommit::FetchedAt, Expr::value(gradient_types::now()))
        .filter(CCommit::Id.eq(commit))
        .filter(CCommit::Message.eq(""))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::EvaluationKind;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    async fn project(db: &TestDatabase) -> ProjectId {
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let project = crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        crate::rows::insert::<ETask, _>(&conn, rows::task())
            .await
            .expect("the task is written");
        project.id
    }

    fn commit(project: Option<ProjectId>, hash: u8, message: &str, author: &str) -> MCommit {
        MCommit {
            id: CommitId::now_v7(),
            message: message.into(),
            hash: vec![hash; 20],
            author_name: author.into(),
            project,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn every_evaluation_of_a_commit_shares_one_row_that_keeps_its_message() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let project = Some(project(&db).await);

        let first = insert_or_update_commit_row_keep_existing_message_if_new_empty(
            &conn,
            commit(project, 7, "fix the build", "Ada"),
        )
        .await
        .expect("the commit is written");
        let again = insert_or_update_commit_row_keep_existing_message_if_new_empty(
            &conn,
            commit(project, 7, "", ""),
        )
        .await
        .expect("the commit is written again");
        let other = insert_or_update_commit_row_keep_existing_message_if_new_empty(
            &conn,
            commit(project, 8, "", ""),
        )
        .await
        .expect("another commit is written");

        assert_eq!(again.id, first.id);
        assert_eq!(
            (again.message.as_str(), again.author_name.as_str()),
            ("fix the build", "Ada")
        );
        assert_ne!(other.id, first.id);

        fill_message_from_git(&conn, other.id, "later".into(), "Grace".into())
            .await
            .expect("the message is filled");
        let filled = db.find::<ECommit>(other.id).await.expect("the row exists");
        assert_eq!(filled.message, "later");
        assert!(filled.fetched_at.is_some());
        db.finish().await;
    }

    #[tokio::test]
    async fn commits_without_a_project_keep_rows_of_their_own() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let a = insert_or_update_commit_row_keep_existing_message_if_new_empty(
            &conn,
            commit(None, 0, "SSH build of a", "Ada"),
        )
        .await
        .expect("the commit is written");
        let b = insert_or_update_commit_row_keep_existing_message_if_new_empty(
            &conn,
            commit(None, 0, "SSH build of b", "Ada"),
        )
        .await
        .expect("the commit is written");
        assert_ne!(a.id, b.id);
        db.finish().await;
    }

    #[tokio::test]
    async fn the_migration_merges_commit_rows_of_one_project_and_hash() {
        use sea_orm_migration::MigratorTrait;

        let db = TestDatabase::create().await;
        let conn = db.connection();
        let project = project(&db).await;
        let placeholder = insert(&conn, commit(None, 1, "", ""))
            .await
            .expect("the placeholder commit is written");
        let mut evaluations = Vec::new();
        for (offset, kind) in [
            (0, EvaluationKind::Normal),
            (1, EvaluationKind::Normal),
            (2, EvaluationKind::Normal),
            (3, EvaluationKind::Ssh),
            (4, EvaluationKind::Ssh),
        ] {
            let evaluation = crate::rows::insert::<EEvaluation, _>(
                &conn,
                MEvaluation {
                    commit: placeholder.id,
                    kind,
                    ..rows::eval_at(EvaluationId::now_v7(), offset)
                },
            )
            .await
            .expect("the evaluation is written");
            evaluations.push(evaluation.id);
        }

        let migrations = gradient_migration::Migrator::migrations();
        let position = migrations
            .iter()
            .position(|m| {
                m.name()
                    == "m20261207_000000_add_abort_record_to_evaluation_and_one_commit_row_per_project_hash"
            })
            .expect("the commit migration is registered");
        let steps = u32::try_from(migrations.len() - position).expect("the count fits in u32");
        gradient_migration::Migrator::down(&conn, Some(steps))
            .await
            .expect("the migrations revert");

        let rows_before: Vec<(CommitId, u8, &str)> = vec![
            (CommitId::now_v7(), 7, ""),
            (CommitId::now_v7(), 7, "fix the build"),
            (CommitId::now_v7(), 7, ""),
            (CommitId::now_v7(), 0, "SSH build of a"),
            (CommitId::now_v7(), 0, "SSH build of b"),
        ];
        for ((id, hash, message), evaluation) in rows_before.iter().zip(&evaluations) {
            db.execute(&format!(
                "INSERT INTO commit (id, message, hash, author_name) \
                 VALUES ('{id}', '{message}', decode(repeat('{hash:02x}', 20), 'hex'), ''); \
                 UPDATE evaluation SET commit = '{id}' WHERE id = '{evaluation}'"
            ))
            .await;
        }

        gradient_migration::Migrator::up(&conn, None)
            .await
            .expect("the migration runs");

        let kept = rows_before[1].0;
        let merged = db
            .find::<ECommit>(kept)
            .await
            .expect("the row with a message stays");
        assert_eq!(merged.project, Some(project));
        assert_eq!(merged.message, "fix the build");
        for (index, (id, _, _)) in rows_before.iter().enumerate() {
            let remains = db.find::<ECommit>(*id).await;
            assert_eq!(remains.is_some(), index != 0 && index != 2, "row {index}");
        }
        for (index, evaluation) in evaluations.iter().enumerate() {
            let row = db
                .find::<EEvaluation>(*evaluation)
                .await
                .expect("the evaluation stays");
            let expected = if index < 3 {
                kept
            } else {
                rows_before[index].0
            };
            assert_eq!(row.commit, expected, "evaluation {index}");
        }
        let ssh = db
            .find::<ECommit>(rows_before[3].0)
            .await
            .expect("the SSH commit stays");
        assert_eq!(ssh.project, None);
        db.finish().await;
    }
}
