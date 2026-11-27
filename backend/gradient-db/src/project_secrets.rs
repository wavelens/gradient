/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use gradient_types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect,
};

pub async fn list<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MProjectSecret>, DbErr> {
    EProjectSecret::find()
        .filter(CProjectSecret::Project.eq(project))
        .order_by_asc(CProjectSecret::Name)
        .all(db)
        .await
}

pub async fn names<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<BTreeSet<String>, DbErr> {
    let names: Vec<String> = EProjectSecret::find()
        .select_only()
        .column(CProjectSecret::Name)
        .filter(CProjectSecret::Project.eq(project))
        .into_tuple()
        .all(db)
        .await?;
    Ok(names.into_iter().collect())
}

pub async fn one<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
) -> Result<Option<MProjectSecret>, DbErr> {
    EProjectSecret::find()
        .filter(CProjectSecret::Project.eq(project))
        .filter(CProjectSecret::Name.eq(name))
        .one(db)
        .await
}

pub async fn upsert<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
    encrypted: String,
    managed: bool,
    updated_by: Option<UserId>,
) -> Result<MProjectSecret, DbErr> {
    let row = AProjectSecret {
        id: Set(ProjectSecretId::now_v7()),
        project: Set(project),
        name: Set(name.to_owned()),
        value: Set(encrypted),
        managed: Set(managed),
        updated_by: Set(updated_by),
        updated_at: Set(gradient_types::now()),
    };
    EProjectSecret::insert(row)
        .on_conflict(
            OnConflict::columns([CProjectSecret::Project, CProjectSecret::Name])
                .update_columns([
                    CProjectSecret::Value,
                    CProjectSecret::Managed,
                    CProjectSecret::UpdatedBy,
                    CProjectSecret::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_with_returning(db)
        .await
}

pub async fn delete<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    name: &str,
) -> Result<bool, DbErr> {
    let result = EProjectSecret::delete_many()
        .filter(CProjectSecret::Project.eq(project))
        .filter(CProjectSecret::Name.eq(name))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

pub async fn delete_managed_except<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    keep: &[String],
) -> Result<u64, DbErr> {
    let result = EProjectSecret::delete_many()
        .filter(CProjectSecret::Project.eq(project))
        .filter(CProjectSecret::Managed.eq(true))
        .filter(CProjectSecret::Name.is_not_in(keep.iter().cloned()))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;

    #[tokio::test]
    async fn project_secret_upsert_replaces_value_list_is_ordered_delete_and_delete_managed_except_are_scoped_to_project_and_managed()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .unwrap();
        let mine = crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .unwrap()
            .id;
        let other = crate::rows::insert::<EProject, _>(
            &conn,
            rows::project_with_id(ProjectId::now_v7(), "other-project"),
        )
        .await
        .unwrap()
        .id;

        upsert(&conn, mine, "wandb-token", "first".into(), false, None)
            .await
            .unwrap();
        let replaced = upsert(&conn, mine, "wandb-token", "second".into(), true, None)
            .await
            .unwrap();
        for (project, name, managed) in [
            (mine, "a-kept", true),
            (mine, "b-dropped", true),
            (mine, "c-unmanaged", false),
            (other, "b-dropped", true),
        ] {
            upsert(&conn, project, name, "v".into(), managed, None)
                .await
                .unwrap();
        }

        let listed: Vec<String> = list(&conn, mine)
            .await
            .unwrap()
            .into_iter()
            .map(|secret| secret.name)
            .collect();
        let removed = delete_managed_except(&conn, mine, &["a-kept".to_owned()])
            .await
            .unwrap();
        let deleted = delete(&conn, mine, "c-unmanaged").await.unwrap();
        let missing = delete(&conn, mine, "c-unmanaged").await.unwrap();

        assert_eq!(
            (replaced.value.as_str(), replaced.managed),
            ("second", true)
        );
        assert_eq!(
            listed,
            ["a-kept", "b-dropped", "c-unmanaged", "wandb-token"]
        );
        assert_eq!(removed, 2, "b-dropped and wandb-token are managed");
        assert!(deleted && !missing);
        assert_eq!(
            names(&conn, mine).await.unwrap(),
            BTreeSet::from(["a-kept".to_owned()])
        );
        assert!(one(&conn, other, "b-dropped").await.unwrap().is_some());
        db.finish().await;
    }
}
