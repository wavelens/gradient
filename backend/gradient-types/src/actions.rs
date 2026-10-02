/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ids::IntegrationId;
use serde::{Deserialize, Serialize};

pub use gradient_entity::task_action::ActionType;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchGeneratorKind {
    #[default]
    FlakeLock,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrGranularity {
    #[default]
    PerRun,
    PerInput,
}

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
    GitHostStatusReport {
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
            ActionConfig::GitHostStatusReport { .. } => ActionType::GitHostStatusReport,
            ActionConfig::OpenPr { .. } => ActionType::OpenPr,
            ActionConfig::SendMatrixMessage { .. } => ActionType::SendMatrixMessage,
            ActionConfig::SendSlackMessage { .. } => ActionType::SendSlackMessage,
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

pub fn is_matrix_room_id(s: &str) -> bool {
    s.strip_prefix('!')
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(local, server)| !local.is_empty() && !server.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(token: Option<&str>) -> ActionConfig {
        ActionConfig::SendMatrixMessage {
            homeserver: "https://matrix.example".into(),
            room_id: "!r:example".into(),
            access_token: token.map(Into::into),
        }
    }

    #[test]
    fn blank_secret_keeps_the_stored_one() {
        let mut edited = matrix(None);
        edited.keep_secret_from(matrix(Some("ENC")));
        assert_eq!(edited, matrix(Some("ENC")));
    }

    #[test]
    fn new_secret_wins_over_the_stored_one() {
        let mut edited = matrix(Some("NEW"));
        edited.keep_secret_from(matrix(Some("ENC")));
        assert_eq!(edited, matrix(Some("NEW")));
    }

    #[test]
    fn secret_field_matches_the_serialized_key() {
        for mut cfg in [
            ActionConfig::SendWebRequest {
                url: "https://x".into(),
                token: None,
            },
            matrix(None),
            ActionConfig::SendSlackMessage { webhook_url: None },
        ] {
            *cfg.secret_mut().unwrap() = Some("S".into());
            let json = serde_json::to_value(&cfg).unwrap();
            let field = cfg.action_type().secret_field().unwrap();
            assert_eq!(json[field], "S");
        }
    }

    #[test]
    fn room_id_needs_bang_and_server() {
        assert!(is_matrix_room_id("!abc:example.org"));
        assert!(!is_matrix_room_id("#ops:example.org"));
        assert!(!is_matrix_room_id("!abc"));
    }
}
