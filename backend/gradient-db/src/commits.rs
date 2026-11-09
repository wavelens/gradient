/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder,
};

pub async fn with_hash<C: ConnectionTrait>(db: &C, hash: Vec<u8>) -> Result<Vec<MCommit>, DbErr> {
    ECommit::find().filter(CCommit::Hash.eq(hash)).all(db).await
}

pub async fn latest_with_message<C: ConnectionTrait>(
    db: &C,
    hash: Vec<u8>,
) -> Result<Option<MCommit>, DbErr> {
    ECommit::find()
        .filter(CCommit::Hash.eq(hash))
        .filter(CCommit::Message.ne(""))
        .order_by_desc(CCommit::Id)
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(db: &C, commit: MCommit) -> Result<MCommit, DbErr> {
    commit.into_active_model().insert(db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;

    #[tokio::test]
    async fn latest_with_message_skips_rows_without_a_message() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let hash = vec![7u8; 20];
        for (message, author_name) in [("older", "Ada"), ("newer", "Grace"), ("", "")] {
            insert(
                &conn,
                MCommit {
                    id: CommitId::now_v7(),
                    message: message.into(),
                    hash: hash.clone(),
                    author_name: author_name.into(),
                    ..Default::default()
                },
            )
            .await
            .expect("the commit is written");
        }

        let known = latest_with_message(&conn, hash)
            .await
            .expect("the lookup runs")
            .expect("a commit with a message exists");
        assert_eq!(
            (known.message.as_str(), known.author_name.as_str()),
            ("newer", "Grace")
        );
        assert!(
            latest_with_message(&conn, vec![8u8; 20])
                .await
                .expect("the lookup runs")
                .is_none()
        );
        db.finish().await;
    }
}
