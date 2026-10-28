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
    use gradient_fixtures::mock::mock_db;

    #[tokio::test]
    async fn an_upsert_replaces_the_value_of_an_existing_name() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_query_results([vec![MProjectSecret {
                id: ProjectSecretId::now_v7(),
                project,
                name: "wandb-token".into(),
                value: "encrypted".into(),
                ..Default::default()
            }]])
            .into_connection();
        upsert(&db, project, "wandb-token", "encrypted".into(), false, None)
            .await
            .expect("upsert");

        let log = db.into_transaction_log();
        let sql = &log[0].statements()[0].sql;
        assert!(
            sql.contains(r#"ON CONFLICT ("project", "name") DO UPDATE"#),
            "{sql}"
        );
        assert!(sql.contains("RETURNING"), "{sql}");
    }
}
