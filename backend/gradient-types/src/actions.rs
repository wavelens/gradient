/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ids::IntegrationId;
use serde::{Deserialize, Serialize};

pub use gradient_entity::task_action::ActionType;

/// Which [`crate::actions`] patch generator an `OpenPr` action runs.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchGeneratorKind {
    #[default]
    FlakeLock,
}

/// How an `OpenPr` action groups bumped inputs into evaluations and PRs.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrGranularity {
    #[default]
    PerRun,
    PerInput,
}

/// The gate an `input_update` evaluation must clear before its PR is opened.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyGate {
    None,
    Eval,
    #[default]
    Build,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    ForgeStatusReport {
        integration_id: IntegrationId,
    },
    OpenPr {
        integration_id: IntegrationId,
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
}

fn default_branch_pattern() -> String {
    "gradient/flake-lock-update".to_owned()
}

fn default_true() -> bool {
    true
}

impl ActionConfig {
    pub fn action_type(&self) -> ActionType {
        match self {
            ActionConfig::SendMail { .. } => ActionType::SendMail,
            ActionConfig::SendWebRequest { .. } => ActionType::SendWebRequest,
            ActionConfig::ForgeStatusReport { .. } => ActionType::ForgeStatusReport,
            ActionConfig::OpenPr { .. } => ActionType::OpenPr,
        }
    }
}
