/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    Order, QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

pub async fn of_task<C: ConnectionTrait>(db: &C, task: TaskId) -> Result<Vec<MTaskAction>, DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Task.eq(task))
        .all(db)
        .await
}

pub async fn by_id_in_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    id: TaskActionId,
) -> Result<Option<MTaskAction>, DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Id.eq(id))
        .filter(CTaskAction::Task.eq(task))
        .one(db)
        .await
}

pub async fn by_name<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    name: &str,
) -> Result<Option<MTaskAction>, DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Task.eq(task))
        .filter(CTaskAction::Name.eq(name))
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(db: &C, action: MTaskAction) -> Result<MTaskAction, DbErr> {
    action.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(
    db: &C,
    action: ATaskAction,
    read_at: NaiveDateTime,
) -> Result<Option<MTaskAction>, DbErr> {
    crate::rows::update_where::<ETaskAction, _>(
        db,
        action,
        Condition::all().add(CTaskAction::UpdatedAt.eq(read_at)),
    )
    .await
}

pub async fn delete<C: ConnectionTrait>(db: &C, action: MTaskAction) -> Result<(), DbErr> {
    action.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn deliveries_of<C: ConnectionTrait>(
    db: &C,
    action: TaskActionId,
    limit: u64,
    offset: u64,
) -> Result<Vec<MTaskActionDelivery>, DbErr> {
    ETaskActionDelivery::find()
        .filter(CTaskActionDelivery::ActionId.eq(action))
        .order_by(CTaskActionDelivery::DeliveredAt, Order::Desc)
        .limit(limit)
        .offset(offset)
        .all(db)
        .await
}

pub async fn latest_deliveries_by_task_action_ids<C: ConnectionTrait>(
    db: &C,
    actions: &[TaskActionId],
) -> Result<HashMap<TaskActionId, MTaskActionDelivery>, DbErr> {
    if actions.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = ETaskActionDelivery::find()
        .distinct_on([CTaskActionDelivery::ActionId])
        .filter(CTaskActionDelivery::ActionId.is_in(actions.iter().copied()))
        .order_by(CTaskActionDelivery::ActionId, Order::Asc)
        .order_by(CTaskActionDelivery::DeliveredAt, Order::Desc)
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|row| (row.action_id, row)).collect())
}

pub async fn delivery_of<C: ConnectionTrait>(
    db: &C,
    action: TaskActionId,
    id: TaskActionDeliveryId,
) -> Result<Option<MTaskActionDelivery>, DbErr> {
    ETaskActionDelivery::find_by_id(id)
        .filter(CTaskActionDelivery::ActionId.eq(action))
        .one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::task_action::ActionType;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::{rows, test_date};

    #[tokio::test]
    async fn latest_deliveries_by_task_action_ids_returns_newest_delivery_and_skips_actions_without_deliveries()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        let task = crate::rows::insert::<ETask, _>(&conn, rows::task())
            .await
            .expect("the task is written");
        let action = |name: &str| MTaskAction {
            id: TaskActionId::now_v7(),
            task: task.id,
            name: name.into(),
            action_type: ActionType::SendWebRequest,
            config: serde_json::json!({}),
            events: serde_json::json!([]),
            active: true,
            created_by: task.created_by,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        };
        let delivered = insert(&conn, action("delivered"))
            .await
            .expect("the action is written");
        let silent = insert(&conn, action("silent"))
            .await
            .expect("the action is written");
        for (minutes, success, status) in [(0, true, 200), (5, false, 503)] {
            crate::rows::insert::<ETaskActionDelivery, _>(
                &conn,
                MTaskActionDelivery {
                    id: TaskActionDeliveryId::now_v7(),
                    action_id: delivered.id,
                    event: "evaluation.completed".into(),
                    response_status: Some(status),
                    success,
                    delivered_at: test_date() + chrono::Duration::minutes(minutes),
                    ..Default::default()
                },
            )
            .await
            .expect("the delivery is written");
        }

        let newest = latest_deliveries_by_task_action_ids(&conn, &[delivered.id, silent.id])
            .await
            .expect("the deliveries load");

        assert_eq!(newest.len(), 1);
        let row = &newest[&delivered.id];
        assert!(!row.success);
        assert_eq!(row.response_status, Some(503));
        db.finish().await;
    }
}
