/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Task Actions dispatch and execution. [`matching_actions`] decides which of a
//! task's actions an event reaches; the execution and per-config executors live
//! in [`executor`] and [`send`].

mod crypto;
mod executor;
mod matchers;
mod payload;
mod report;
mod send;

use crate::context::CiContext;
use gradient_types::{ActionType, CTaskAction, ETaskAction, TaskId};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value as JsonValue;

pub use crypto::{
    decrypt_action_secret, decrypt_secret_with_file, encrypt_action_secret,
    encrypt_secret_with_file,
};
pub use executor::execute_action;
pub use matchers::{FORGE_STATUS_EVENTS, forge_status_for_event, matches_event};
pub use payload::forge_status_payload;
pub use send::{reporter_for_task, verify_forge_action};

pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Successful action delivery: the executor's HTTP/SMTP status and any response
/// body, recorded on the `task_action_delivery` row.
pub(crate) struct ExecutorOk {
    pub(crate) status_code: Option<i32>,
    pub(crate) response_body: Option<String>,
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        if let Some((idx, _)) = s.char_indices().take_while(|(i, _)| *i <= max).last() {
            s.truncate(idx);
        } else {
            s.truncate(max);
        }
    }
    s
}

/// The active actions of `task_id`, in insertion order.
pub async fn active_actions_for_task(
    ctx: &CiContext,
    task_id: TaskId,
) -> Result<Vec<gradient_types::MTaskAction>, sea_orm::DbErr> {
    ETaskAction::find()
        .filter(CTaskAction::Task.eq(task_id))
        .filter(CTaskAction::Active.eq(true))
        .all(&ctx.db.worker_db)
        .await
}

/// The actions that react to `event`, with the two payload rules: `OpenPr` only
/// on input-update evaluations, forge reports never on them.
///
/// `OpenPr` fires on a normal gate event (build/eval completed) but must only
/// act on `input_update` evaluations, never regular CI runs. A
/// `forge_status_report` posts a CI status against a real commit/PR, and an
/// `input_update` eval is an internal bump whose own commit is blank until its
/// PR is pushed, so it is skipped there: the PR's own CI run reports normally.
pub fn matching_actions(
    actions: Vec<gradient_types::MTaskAction>,
    event: &str,
    payload: &JsonValue,
) -> Vec<gradient_types::MTaskAction> {
    let is_input_update =
        payload.get("evaluation_kind").and_then(|v| v.as_str()) == Some("input_update");

    actions
        .into_iter()
        .filter(|a| matches_event(a, event))
        .filter(|a| a.action_type != ActionType::OpenPr || is_input_update)
        .filter(|a| a.action_type != ActionType::ForgeStatusReport || !is_input_update)
        .collect()
}

#[cfg(test)]
mod tests;
