/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
pub use matchers::{GIT_HOST_STATUS_EVENTS, git_host_status_for_event, matches_event};
pub use payload::git_host_status_payload;
pub use send::{reporter_for_task, verify_git_host_action};

pub const MAX_BODY_BYTES: usize = 64 * 1024;

pub(crate) struct ExecutorOk {
    pub(crate) status_code: Option<i32>,
    pub(crate) response_body: Option<String>,
}

pub(crate) fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        if let Some((idx, _)) = s.char_indices().take_while(|(i, _)| *i <= max).last() {
            s.truncate(idx);
        } else {
            s.truncate(max);
        }
    }
    s
}

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

/// `OpenPr` must act only on `input_update` evaluations. Git host reports must skip them because
/// their commit is blank until the PR is pushed. The PR's own CI run is reporting normally.
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
        .filter(|a| a.action_type != ActionType::GitHostStatusReport || !is_input_update)
        .collect()
}

#[cfg(test)]
mod tests;
