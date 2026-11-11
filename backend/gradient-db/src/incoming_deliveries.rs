/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::incoming_webhook_delivery::{Column, Entity, Model};
use gradient_types::GitHostConnectionId;
use sea_orm::sea_query::Query;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, Order, QueryFilter,
    QueryOrder, QuerySelect,
};

const KEPT_PER_CONNECTION: u64 = 50;

pub async fn record<C: ConnectionTrait>(db: &C, delivery: Model) -> Result<(), DbErr> {
    let connection = delivery.git_host_connection;
    Entity::insert(delivery.into_active_model())
        .exec_without_returning(db)
        .await?;
    let newest = Query::select()
        .column(Column::Id)
        .from(Entity)
        .and_where(Column::GitHostConnection.eq(connection))
        .order_by(Column::CreatedAt, Order::Desc)
        .limit(KEPT_PER_CONNECTION)
        .to_owned();
    Entity::delete_many()
        .filter(Column::GitHostConnection.eq(connection))
        .filter(Column::Id.not_in_subquery(newest))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn of_connection<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    limit: u64,
) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .filter(Column::GitHostConnection.eq(connection))
        .order_by_desc(Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
}
