/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The `cron` crate (v0.16) is expecting six-field expressions `sec min hour dom mon dow`.
//! The five-field POSIX form is invalid here.

use crate::ids::IntegrationId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use gradient_entity::task::ConcurrencyPolicy;
pub use gradient_entity::task_trigger::TriggerType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    Polling {
        interval_secs: u32,
        #[serde(default)]
        branch: Option<String>,
    },
    ReporterPush {
        integration_id: IntegrationId,
        #[serde(default)]
        branches: Vec<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        releases_only: bool,
    },
    ReporterPullRequest {
        integration_id: IntegrationId,
        #[serde(default)]
        branches: Vec<String>,
        #[serde(default = "default_pr_actions")]
        actions: Vec<String>,
        #[serde(default = "default_require_approval")]
        require_approval: bool,
    },
    Time {
        cron: String,
    },
}

fn default_pr_actions() -> Vec<String> {
    vec!["opened".into(), "synchronize".into(), "reopened".into()]
}

fn default_require_approval() -> bool {
    true
}

#[derive(Debug, Error)]
pub enum TriggerConfigError {
    #[error("polling interval_secs must be at least 10")]
    PollingIntervalTooSmall,
    #[error("invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("malformed config: {0}")]
    Malformed(#[from] serde_json::Error),
}

impl TriggerConfig {
    pub fn trigger_type(&self) -> TriggerType {
        match self {
            Self::Polling { .. } => TriggerType::Polling,
            Self::ReporterPush { .. } => TriggerType::ReporterPush,
            Self::ReporterPullRequest { .. } => TriggerType::ReporterPullRequest,
            Self::Time { .. } => TriggerType::Time,
        }
    }

    pub fn parse_row(
        trigger_type: TriggerType,
        config: &serde_json::Value,
    ) -> Result<Self, TriggerConfigError> {
        let mut value = config.clone();
        if let serde_json::Value::Object(ref mut m) = value {
            m.insert("type".into(), serde_json::to_value(trigger_type)?);
        }
        let parsed: TriggerConfig = serde_json::from_value(value)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), TriggerConfigError> {
        match self {
            Self::Polling { interval_secs, .. } if *interval_secs < 10 => {
                Err(TriggerConfigError::PollingIntervalTooSmall)
            }
            Self::Time { cron } => {
                cron.parse::<cron::Schedule>()
                    .map_err(|e| TriggerConfigError::InvalidCron(e.to_string()))?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn to_db_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self)
            .expect("a TriggerConfig is a derived enum of strings and numbers");
        if let serde_json::Value::Object(ref mut m) = v {
            m.remove("type");
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_config_round_trip() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
        };
        let db = cfg.to_db_json();
        assert!(
            db.get("type").is_none(),
            "db json should not carry the type tag"
        );
        let parsed = TriggerConfig::parse_row(TriggerType::Polling, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn polling_config_with_branch_round_trip() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 120,
            branch: Some("develop".into()),
        };
        let db = cfg.to_db_json();
        assert_eq!(db["branch"], serde_json::json!("develop"));
        let parsed = TriggerConfig::parse_row(TriggerType::Polling, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn polling_under_10_seconds_rejected() {
        let cfg = TriggerConfig::Polling {
            interval_secs: 5,
            branch: None,
        };
        assert!(matches!(
            cfg.validate(),
            Err(TriggerConfigError::PollingIntervalTooSmall)
        ));
    }

    #[test]
    fn cron_invalid_rejected() {
        let cfg = TriggerConfig::Time {
            cron: "not a cron".into(),
        };
        assert!(matches!(
            cfg.validate(),
            Err(TriggerConfigError::InvalidCron(_))
        ));
    }

    #[test]
    fn cron_valid_accepted() {
        let cfg = TriggerConfig::Time {
            cron: "0 0 2 * * *".into(),
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn type_mismatch_rejected() {
        let bad = serde_json::json!({"interval_secs": 60});
        let res = TriggerConfig::parse_row(TriggerType::Time, &bad);
        assert!(res.is_err(), "expected error, got {res:?}");
    }

    #[test]
    fn reporter_pull_request_require_approval_defaults_true_for_legacy_rows() {
        // Pre-#247 rows are lacking `require_approval` in the stored JSON. The serde default must
        // read as `true` to keep them secure by default without a backfill migration.
        let legacy_db = serde_json::json!({
            "integration_id": IntegrationId::nil(),
            "branches": [],
            "actions": ["opened"],
        });
        let parsed =
            TriggerConfig::parse_row(TriggerType::ReporterPullRequest, &legacy_db).unwrap();
        let TriggerConfig::ReporterPullRequest {
            require_approval, ..
        } = parsed
        else {
            panic!("expected ReporterPullRequest");
        };
        assert!(require_approval, "missing field must default to true");
    }
}
