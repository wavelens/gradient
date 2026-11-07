/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::incoming_webhook_delivery::{Column, Entity, Model};
use gradient_types::IntegrationId;
use sea_orm::sea_query::Query;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, Order, QueryFilter,
    QueryOrder, QuerySelect,
};

const KEPT_PER_INTEGRATION: u64 = 50;

pub async fn record<C: ConnectionTrait>(db: &C, delivery: Model) -> Result<(), DbErr> {
    let integration = delivery.integration;
    Entity::insert(delivery.into_active_model())
        .exec_without_returning(db)
        .await?;
    let newest = Query::select()
        .column(Column::Id)
        .from(Entity)
        .and_where(Column::Integration.eq(integration))
        .order_by(Column::CreatedAt, Order::Desc)
        .limit(KEPT_PER_INTEGRATION)
        .to_owned();
    Entity::delete_many()
        .filter(Column::Integration.eq(integration))
        .filter(Column::Id.not_in_subquery(newest))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn of_integration<C: ConnectionTrait>(
    db: &C,
    integration: IntegrationId,
    limit: u64,
) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .filter(Column::Integration.eq(integration))
        .order_by_desc(Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
}
