/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::webhook::WebhookScope;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, FromQueryResult,
    IntoActiveModel, Order, QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

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

#[derive(Clone, Debug, FromQueryResult)]
pub struct NewestWebhookDeliveryRow {
    pub webhook_id: WebhookId,
    pub success: bool,
    pub delivered_at: chrono::NaiveDateTime,
    pub response_status: Option<i32>,
    pub error_message: Option<String>,
}

pub async fn latest_delivery_summaries_by_webhook_ids<C: ConnectionTrait>(
    db: &C,
    webhooks: &[WebhookId],
) -> Result<HashMap<WebhookId, NewestWebhookDeliveryRow>, DbErr> {
    if webhooks.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = EWebhookDelivery::find()
        .select_only()
        .columns([
            CWebhookDelivery::WebhookId,
            CWebhookDelivery::Success,
            CWebhookDelivery::DeliveredAt,
            CWebhookDelivery::ResponseStatus,
            CWebhookDelivery::ErrorMessage,
        ])
        .distinct_on([CWebhookDelivery::WebhookId])
        .filter(CWebhookDelivery::WebhookId.is_in(webhooks.iter().copied()))
        .order_by(CWebhookDelivery::WebhookId, Order::Asc)
        .order_by(CWebhookDelivery::DeliveredAt, Order::Desc)
        .into_model::<NewestWebhookDeliveryRow>()
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|row| (row.webhook_id, row)).collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::{rows, test_date};

    #[tokio::test]
    async fn latest_delivery_summaries_returns_only_the_most_recent_delivery_per_webhook_and_skips_webhooks_without_deliveries()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let webhook = |name: &str| MWebhook {
            id: WebhookId::now_v7(),
            scope: WebhookScope::Instance,
            name: name.into(),
            url: "https://example.com/hook".into(),
            events: serde_json::json!([]),
            created_by: owner.id,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };
        let delivered = crate::rows::insert::<EWebhook, _>(&conn, webhook("delivered"))
            .await
            .expect("the webhook is written");
        let silent = crate::rows::insert::<EWebhook, _>(&conn, webhook("silent"))
            .await
            .expect("the webhook is written");
        for (minutes, success, status) in [(0, true, 200), (5, false, 503)] {
            crate::rows::insert::<EWebhookDelivery, _>(
                &conn,
                MWebhookDelivery {
                    id: WebhookDeliveryId::now_v7(),
                    webhook_id: delivered.id,
                    event: "webhook.ping".into(),
                    response_status: Some(status),
                    success,
                    delivered_at: test_date() + chrono::Duration::minutes(minutes),
                    ..Default::default()
                },
            )
            .await
            .expect("the delivery is written");
        }

        let newest = latest_delivery_summaries_by_webhook_ids(&conn, &[delivered.id, silent.id])
            .await
            .expect("the summaries load");

        assert_eq!(newest.len(), 1);
        let row = &newest[&delivered.id];
        assert!(!row.success);
        assert_eq!(row.response_status, Some(503));
        db.finish().await;
    }
}
