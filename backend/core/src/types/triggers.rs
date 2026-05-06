/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed wrappers around the `project_trigger` table's enum/jsonb columns.
//!
//! The `cron` crate (v0.12) expects **six-field** expressions:
//! `sec min hour dom mon dow` — not the five-field POSIX form.

use crate::types::ids::IntegrationId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Polling,
    ReporterPush,
    ReporterPullRequest,
    Time,
}

impl TriggerType {
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Polling => 0,
            Self::ReporterPush => 1,
            Self::ReporterPullRequest => 2,
            Self::Time => 3,
        }
    }

    pub fn from_i16(v: i16) -> Option<Self> {
        Some(match v {
            0 => Self::Polling,
            1 => Self::ReporterPush,
            2 => Self::ReporterPullRequest,
            3 => Self::Time,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    HardAbort,
    SoftAbort,
    Allow,
    Skip,
}

impl ConcurrencyPolicy {
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::HardAbort => 0,
            Self::SoftAbort => 1,
            Self::Allow => 2,
            Self::Skip => 3,
        }
    }

    pub fn from_i16(v: i16) -> Option<Self> {
        Some(match v {
            0 => Self::HardAbort,
            1 => Self::SoftAbort,
            2 => Self::Allow,
            3 => Self::Skip,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    Polling {
        interval_secs: u32,
        /// Branch to poll (`refs/heads/<branch>`). `None` polls the remote HEAD
        /// (the repo's default branch).
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
    },
    Time { cron: String },
}

fn default_pr_actions() -> Vec<String> {
    vec!["opened".into(), "synchronize".into(), "reopened".into()]
}

#[derive(Debug, Error)]
pub enum TriggerConfigError {
    #[error("polling interval_secs must be at least 10")]
    PollingIntervalTooSmall,
    #[error("invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("malformed config: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("trigger_type {0} does not match config shape")]
    TypeMismatch(i16),
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

    /// Parse a row's `(trigger_type, config_json)` pair into a typed config.
    pub fn parse_row(trigger_type: i16, config: &serde_json::Value) -> Result<Self, TriggerConfigError> {
        let tag = TriggerType::from_i16(trigger_type).ok_or(TriggerConfigError::TypeMismatch(trigger_type))?;
        let mut value = config.clone();
        if let serde_json::Value::Object(ref mut m) = value {
            m.insert(
                "type".into(),
                serde_json::Value::String(
                    match tag {
                        TriggerType::Polling => "polling",
                        TriggerType::ReporterPush => "reporter_push",
                        TriggerType::ReporterPullRequest => "reporter_pull_request",
                        TriggerType::Time => "time",
                    }
                    .into(),
                ),
            );
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

    /// Serialise to the JSON shape stored in the DB (without the `"type"` tag).
    pub fn to_db_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap();
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
        let cfg = TriggerConfig::Polling { interval_secs: 60, branch: None };
        let db = cfg.to_db_json();
        assert!(db.get("type").is_none(), "db json should not carry the type tag");
        let parsed = TriggerConfig::parse_row(0, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn polling_config_with_branch_round_trip() {
        let cfg = TriggerConfig::Polling { interval_secs: 120, branch: Some("develop".into()) };
        let db = cfg.to_db_json();
        assert_eq!(db["branch"], serde_json::json!("develop"));
        let parsed = TriggerConfig::parse_row(0, &db).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn polling_under_10_seconds_rejected() {
        let cfg = TriggerConfig::Polling { interval_secs: 5, branch: None };
        assert!(matches!(cfg.validate(), Err(TriggerConfigError::PollingIntervalTooSmall)));
    }

    #[test]
    fn cron_invalid_rejected() {
        let cfg = TriggerConfig::Time { cron: "not a cron".into() };
        assert!(matches!(cfg.validate(), Err(TriggerConfigError::InvalidCron(_))));
    }

    #[test]
    fn cron_valid_accepted() {
        // The `cron` crate uses six-field expressions: sec min hour dom mon dow.
        let cfg = TriggerConfig::Time { cron: "0 0 2 * * *".into() };
        cfg.validate().unwrap();
    }

    #[test]
    fn type_mismatch_rejected() {
        // Polling-shaped config passed for trigger_type=time (cron field missing).
        let bad = serde_json::json!({"interval_secs": 60});
        let res = TriggerConfig::parse_row(3, &bad);
        assert!(res.is_err(), "expected error, got {res:?}");
    }

    #[test]
    fn concurrency_round_trip() {
        for (p, n) in [
            (ConcurrencyPolicy::HardAbort, 0),
            (ConcurrencyPolicy::SoftAbort, 1),
            (ConcurrencyPolicy::Allow, 2),
            (ConcurrencyPolicy::Skip, 3),
        ] {
            assert_eq!(p.as_i16(), n);
            assert_eq!(ConcurrencyPolicy::from_i16(n), Some(p));
        }
    }

    #[test]
    fn trigger_type_round_trip() {
        for (t, n) in [
            (TriggerType::Polling, 0),
            (TriggerType::ReporterPush, 1),
            (TriggerType::ReporterPullRequest, 2),
            (TriggerType::Time, 3),
        ] {
            assert_eq!(t.as_i16(), n);
            assert_eq!(TriggerType::from_i16(n), Some(t));
        }
    }

    #[test]
    fn reporter_push_db_json_omits_type_tag() {
        let id = IntegrationId::nil();
        let cfg = TriggerConfig::ReporterPush {
            integration_id: id,
            branches: vec!["main".into()],
            tags: vec![],
            releases_only: false,
        };
        let db = cfg.to_db_json();
        assert!(db.get("type").is_none(), "db json should not carry the type tag");
        let parsed = TriggerConfig::parse_row(1, &db).unwrap();
        assert_eq!(parsed, cfg);
    }
}
