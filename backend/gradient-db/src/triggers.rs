/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::triggers::TriggerType;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult,
    IntoActiveModel, QueryFilter, Value, sea_query::Expr,
};

pub async fn of_task<C: ConnectionTrait>(db: &C, task: TaskId) -> Result<Vec<MTaskTrigger>, DbErr> {
    ETaskTrigger::find()
        .filter(CTaskTrigger::Task.eq(task))
        .all(db)
        .await
}

pub async fn of_tasks<C: ConnectionTrait>(
    db: &C,
    tasks: &[TaskId],
) -> Result<Vec<MTaskTrigger>, DbErr> {
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    ETaskTrigger::find()
        .filter(CTaskTrigger::Task.is_in(tasks.iter().copied()))
        .all(db)
        .await
}

pub async fn by_id_in_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    id: TaskTriggerId,
) -> Result<Option<MTaskTrigger>, DbErr> {
    ETaskTrigger::find()
        .filter(CTaskTrigger::Id.eq(id))
        .filter(CTaskTrigger::Task.eq(task))
        .one(db)
        .await
}

pub async fn default_of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<Option<MTaskTrigger>, DbErr> {
    ETaskTrigger::find()
        .filter(CTaskTrigger::Task.eq(task))
        .filter(CTaskTrigger::IsDefault.eq(true))
        .one(db)
        .await
}

pub async fn make_default<C: ConnectionTrait>(
    db: &C,
    trigger: MTaskTrigger,
) -> Result<MTaskTrigger, DbErr> {
    ETaskTrigger::update_many()
        .col_expr(CTaskTrigger::IsDefault, Expr::value(false))
        .filter(CTaskTrigger::Task.eq(trigger.task))
        .filter(CTaskTrigger::IsDefault.eq(true))
        .exec(db)
        .await?;
    let mut active: ATaskTrigger = trigger.into();
    active.is_default = Set(true);
    active.updated_at = Set(gradient_types::now());
    active.update(db).await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    trigger: MTaskTrigger,
) -> Result<MTaskTrigger, DbErr> {
    trigger.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(
    db: &C,
    trigger: ATaskTrigger,
) -> Result<MTaskTrigger, DbErr> {
    trigger.update(db).await
}

pub async fn delete<C: ConnectionTrait>(db: &C, trigger: MTaskTrigger) -> Result<(), DbErr> {
    trigger.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn set_last_fired<C: ConnectionTrait>(
    db: &C,
    trigger: MTaskTrigger,
    at: NaiveDateTime,
) -> Result<MTaskTrigger, DbErr> {
    let mut active: ATaskTrigger = trigger.into();
    active.last_fired_at = Set(Some(at));
    active.updated_at = Set(at);
    active.update(db).await
}

fn active_triggers_sql(trigger_type: TriggerType) -> String {
    format!(
        "SELECT pt.* FROM task_trigger pt \
         JOIN task p ON pt.task = p.id \
         WHERE pt.active = true \
           AND p.active = true \
           AND pt.trigger_type = {} \
           AND p.git_host_connection = $1",
        i16::from(trigger_type),
    )
}

crate::sql_fn! {
    ACTIVE_TRIGGERS_FOR_TASKS_USING_GIT_HOST_CONNECTION = || active_triggers_sql(TriggerType::ReporterPullRequest),
        params = [GitHostConnectionId];
}

pub async fn list_active_triggers_of_type_on_tasks_using_git_host_connection<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    trigger_type: TriggerType,
) -> Result<Vec<MTaskTrigger>, DbErr> {
    let stmt = ACTIVE_TRIGGERS_FOR_TASKS_USING_GIT_HOST_CONNECTION.bind_built(
        active_triggers_sql(trigger_type),
        [Value::Uuid(Some(connection.into_inner()))],
    );
    ETaskTrigger::find().from_raw_sql(stmt).all(db).await
}

crate::sql! {
    ACTIVE_PULL_REQUEST_TASK_IDS_FOR_GIT_HOST_CONNECTION = "SELECT DISTINCT tt.task FROM task_trigger tt \
         JOIN task t ON t.id = tt.task \
         WHERE tt.active = true \
           AND t.active = true \
           AND tt.trigger_type = $1 \
           AND t.git_host_connection = $2",
        params = [Int(2), GitHostConnectionId];
}

pub async fn active_pull_request_task_ids_using_git_host_connection<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
) -> Result<Vec<TaskId>, DbErr> {
    let stmt = ACTIVE_PULL_REQUEST_TASK_IDS_FOR_GIT_HOST_CONNECTION.bind([
        Value::SmallInt(Some(i16::from(TriggerType::ReporterPullRequest))),
        Value::Uuid(Some(connection.into_inner())),
    ]);

    #[derive(FromQueryResult)]
    struct TaskRow {
        task: TaskId,
    }
    TaskRow::find_by_statement(stmt)
        .all(db)
        .await
        .map(|rows| rows.into_iter().map(|r| r.task).collect())
}
