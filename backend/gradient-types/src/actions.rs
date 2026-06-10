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
}

impl ActionType {
    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            0 => Some(Self::SendMail),
            1 => Some(Self::SendWebRequest),
            2 => Some(Self::ForgeStatusReport),
            _ => None,
        }
    }

    pub fn to_i16(self) -> i16 {
        self as i16
    }
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
}

impl ActionConfig {
    pub fn action_type(&self) -> ActionType {
        match self {
            ActionConfig::SendMail { .. } => ActionType::SendMail,
            ActionConfig::SendWebRequest { .. } => ActionType::SendWebRequest,
            ActionConfig::ForgeStatusReport { .. } => ActionType::ForgeStatusReport,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

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
        let id = IntegrationId::from(Uuid::new_v4());
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
        ] {
            assert_eq!(ActionType::from_i16(at.to_i16()), Some(at));
        }
        assert_eq!(ActionType::from_i16(99), None);
    }
}
