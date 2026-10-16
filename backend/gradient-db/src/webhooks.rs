/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::webhook::WebhookScope;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    Order, QueryFilter, QueryOrder, QuerySelect,
};

#[derive(Clone, Copy, Debug)]
pub struct Owner {
    pub scope: WebhookScope,
    pub project: Option<ProjectId>,
    pub cache: Option<CacheId>,
    pub team: Option<TeamId>,
}

impl Owner {
    fn rows(self) -> Condition {
        Condition::all()
            .add(CWebhook::Scope.eq(self.scope))
            .add(match self.project {
                Some(p) => CWebhook::Project.eq(p),
                None => CWebhook::Project.is_null(),
            })
            .add(match self.cache {
                Some(c) => CWebhook::Cache.eq(c),
                None => CWebhook::Cache.is_null(),
            })
            .add(match self.team {
                Some(t) => CWebhook::Team.eq(t),
                None => CWebhook::Team.is_null(),
            })
    }
}

pub async fn of_owner<C: ConnectionTrait>(db: &C, owner: Owner) -> Result<Vec<MWebhook>, DbErr> {
    EWebhook::find()
        .filter(owner.rows())
        .order_by_asc(CWebhook::Name)
        .all(db)
        .await
}

pub async fn by_id_of_owner<C: ConnectionTrait>(
    db: &C,
    owner: Owner,
    id: WebhookId,
) -> Result<Option<MWebhook>, DbErr> {
    EWebhook::find_by_id(id).filter(owner.rows()).one(db).await
}

pub async fn by_name_of_owner<C: ConnectionTrait>(
    db: &C,
    owner: Owner,
    name: &str,
) -> Result<Option<MWebhook>, DbErr> {
    EWebhook::find()
        .filter(owner.rows())
        .filter(CWebhook::Name.eq(name))
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(db: &C, webhook: MWebhook) -> Result<MWebhook, DbErr> {
    webhook.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(db: &C, webhook: AWebhook) -> Result<MWebhook, DbErr> {
    webhook.update(db).await
}

pub async fn delete<C: ConnectionTrait>(db: &C, webhook: MWebhook) -> Result<(), DbErr> {
    webhook.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn deliveries_of<C: ConnectionTrait>(
    db: &C,
    webhook: WebhookId,
    limit: u64,
    offset: u64,
) -> Result<Vec<MWebhookDelivery>, DbErr> {
    EWebhookDelivery::find()
        .filter(CWebhookDelivery::WebhookId.eq(webhook))
        .order_by(CWebhookDelivery::DeliveredAt, Order::Desc)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await
}

pub async fn delivery_of<C: ConnectionTrait>(
    db: &C,
    webhook: WebhookId,
    id: WebhookDeliveryId,
) -> Result<Option<MWebhookDelivery>, DbErr> {
    EWebhookDelivery::find_by_id(id)
        .filter(CWebhookDelivery::WebhookId.eq(webhook))
        .one(db)
        .await
}
