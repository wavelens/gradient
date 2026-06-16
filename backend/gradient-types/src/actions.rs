/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ids::IntegrationId;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum ActionType {
    SendMail = 0,
    SendWebRequest = 1,
    ForgeStatusReport = 2,
    OpenPr = 3,
}

impl ActionType {
    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            0 => Some(Self::SendMail),
            1 => Some(Self::SendWebRequest),
            2 => Some(Self::ForgeStatusReport),
            3 => Some(Self::OpenPr),
            _ => None,
        }
    }

    pub fn to_i16(self) -> i16 {
        self as i16
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_mail_round_trip() {
        let cfg = ActionConfig::SendMail {
            recipients: vec!["ops@example.com".into()],
            subject_template: Some("[Gradient] {event}".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"type\":\"send_mail\""));
        let back: ActionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(cfg.action_type(), ActionType::SendMail);
    }

    #[test]
    fn send_web_request_token_skipped_when_none() {
        let cfg = ActionConfig::SendWebRequest {
            url: "https://example.com/hook".into(),
            token: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("token"));
        let back: ActionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn forge_status_report_carries_integration_id() {
        let id = IntegrationId::now_v7();
        let cfg = ActionConfig::ForgeStatusReport { integration_id: id };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ActionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn action_type_i16_round_trip() {
        for at in [
            ActionType::SendMail,
            ActionType::SendWebRequest,
            ActionType::ForgeStatusReport,
            ActionType::OpenPr,
        ] {
            assert_eq!(ActionType::from_i16(at.to_i16()), Some(at));
        }
        assert_eq!(ActionType::from_i16(99), None);
    }

    #[test]
    fn open_pr_defaults_apply() {
        let json = serde_json::json!({
            "type": "open_pr",
            "integration_id": Uuid::new_v4(),
        });
        let cfg: ActionConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.action_type(), ActionType::OpenPr);
        match cfg {
            ActionConfig::OpenPr {
                generator,
                granularity,
                verify_gate,
                branch_pattern,
                update_existing,
                ..
            } => {
                assert_eq!(generator, PatchGeneratorKind::FlakeLock);
                assert_eq!(granularity, PrGranularity::PerRun);
                assert_eq!(verify_gate, VerifyGate::Build);
                assert_eq!(branch_pattern, "gradient/flake-lock-update");
                assert!(update_existing);
            }
            _ => panic!("expected OpenPr"),
        }
    }

    #[test]
    fn open_pr_round_trip() {
        let cfg = ActionConfig::OpenPr {
            integration_id: IntegrationId::from(Uuid::new_v4()),
            generator: PatchGeneratorKind::FlakeLock,
            granularity: PrGranularity::PerInput,
            verify_gate: VerifyGate::Eval,
            branch_pattern: "gradient/flake-lock-update/{input}".into(),
            title_template: Some("chore: bump {input}".into()),
            body_template: None,
            update_existing: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"type\":\"open_pr\""));
        let back: ActionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }
}
