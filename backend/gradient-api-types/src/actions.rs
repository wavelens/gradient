/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[repr(i16)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(
        sea_orm::DeriveActiveEnum,
        sea_orm::EnumIter,
        num_enum::IntoPrimitive,
        num_enum::TryFromPrimitive
    )
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
pub enum ActionType {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    SendMail = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    SendWebRequest = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    OpenPr = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    SendMatrixMessage = 4,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 5)
    )]
    SendSlackMessage = 5,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 6)
    )]
    StartEvaluation = 6,
}

impl ActionType {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionType::SendMail => "send_mail",
            ActionType::SendWebRequest => "send_web_request",
            ActionType::OpenPr => "open_pr",
            ActionType::SendMatrixMessage => "send_matrix_message",
            ActionType::SendSlackMessage => "send_slack_message",
            ActionType::StartEvaluation => "start_evaluation",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::SendMail,
            Self::SendWebRequest,
            Self::OpenPr,
            Self::SendMatrixMessage,
            Self::SendSlackMessage,
            Self::StartEvaluation,
        ]
        .into_iter()
        .find(|t| t.as_str() == name)
    }

    pub fn secret_field(self) -> Option<&'static str> {
        match self {
            ActionType::SendWebRequest => Some("token"),
            ActionType::SendMatrixMessage => Some("access_token"),
            ActionType::SendSlackMessage => Some("webhook_url"),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatchGeneratorKind {
    #[default]
    FlakeLock,
    ExecTree,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrGranularity {
    #[default]
    PerRun,
    PerInput,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerifyGate {
    None,
    Eval,
    #[default]
    Build,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionConfig {
    SendMail {
        recipients: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject_template: Option<String>,
    },
    SendWebRequest {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    OpenPr {
        git_host_connection_id: GitHostConnectionId,
        #[serde(default)]
        generator: PatchGeneratorKind,
        #[serde(default)]
        granularity: PrGranularity,
        #[serde(default)]
        verify_gate: VerifyGate,
        #[serde(default = "default_branch_pattern")]
        branch_pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title_template: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_template: Option<String>,
        #[serde(default = "default_true")]
        update_existing: bool,
    },
    SendMatrixMessage {
        homeserver: String,
        room_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access_token: Option<String>,
    },
    SendSlackMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        webhook_url: Option<String>,
    },
    StartEvaluation {
        task: TaskId,
    },
}

impl ActionConfig {
    pub fn to_db_json(&self) -> serde_json::Value {
        serde_json::to_value(self)
            .expect("ActionConfig is a derived enum of strings, IDs and flags.")
    }

    pub fn action_type(&self) -> ActionType {
        match self {
            ActionConfig::SendMail { .. } => ActionType::SendMail,
            ActionConfig::SendWebRequest { .. } => ActionType::SendWebRequest,
            ActionConfig::OpenPr { .. } => ActionType::OpenPr,
            ActionConfig::SendMatrixMessage { .. } => ActionType::SendMatrixMessage,
            ActionConfig::SendSlackMessage { .. } => ActionType::SendSlackMessage,
            ActionConfig::StartEvaluation { .. } => ActionType::StartEvaluation,
        }
    }

    pub fn secret_mut(&mut self) -> Option<&mut Option<String>> {
        match self {
            ActionConfig::SendWebRequest { token, .. } => Some(token),
            ActionConfig::SendMatrixMessage { access_token, .. } => Some(access_token),
            ActionConfig::SendSlackMessage { webhook_url } => Some(webhook_url),
            _ => None,
        }
    }

    pub fn keep_secret_from(&mut self, mut stored: ActionConfig) {
        if let Some(slot) = self.secret_mut()
            && slot.is_none()
            && let Some(old) = stored.secret_mut()
        {
            *slot = old.take();
        }
    }
}

fn default_branch_pattern() -> String {
    "gradient/flake-lock-update".to_owned()
}
