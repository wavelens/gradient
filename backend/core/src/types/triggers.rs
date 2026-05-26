/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed wrappers around the `project_trigger` table's enum/jsonb columns.
//!
//! The `cron` crate (v0.12) expects **six-field** expressions:
//! `sec min hour dom mon dow` - not the five-field POSIX form.

use crate::types::ids::IntegrationId;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[repr(i16)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, IntoPrimitive, TryFromPrimitive,
)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Polling = 0,
    ReporterPush = 1,
    ReporterPullRequest = 2,
    Time = 3,
}

#[repr(i16)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, IntoPrimitive, TryFromPrimitive,
)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    HardAbort = 0,
    SoftAbort = 1,
    All = 2,
    Skip = 3,
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
        /// When true (default, secure-by-default), PRs from contributors who
        /// are not repo writers on the forge are parked in
        /// `WaitingReason::Approval` until a maintainer either clicks the
        /// "Approve and Run" check-run action (GitHub) or comments
        /// `/gradient approve` (or `/gradient run`) on the PR.
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
    pub fn parse_row(
        trigger_type: i16,
        config: &serde_json::Value,
    ) -> Result<Self, TriggerConfigError> {
        let tag = TriggerType::try_from(trigger_type)
            .map_err(|_| TriggerConfigError::TypeMismatch(trigger_type))?;
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
        let cfg = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
        };
        let db = cfg.to_db_json();
        assert!(
            db.get("type").is_none(),
            "db json should not carry the type tag"
        );
        let parsed = TriggerConfig::parse_row(0, &db).unwrap();
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
        let parsed = TriggerConfig::parse_row(0, &db).unwrap();
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
        // The `cron` crate uses six-field expressions: sec min hour dom mon dow.
        let cfg = TriggerConfig::Time {
            cron: "0 0 2 * * *".into(),
        };
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
            (ConcurrencyPolicy::All, 2),
            (ConcurrencyPolicy::Skip, 3),
        ] {
            assert_eq!(i16::from(p), n);
            assert_eq!(ConcurrencyPolicy::try_from(n), Ok(p));
        }
        assert!(ConcurrencyPolicy::try_from(99).is_err());
    }

    #[test]
    fn trigger_type_round_trip() {
        for (t, n) in [
            (TriggerType::Polling, 0),
            (TriggerType::ReporterPush, 1),
            (TriggerType::ReporterPullRequest, 2),
            (TriggerType::Time, 3),
        ] {
            assert_eq!(i16::from(t), n);
            assert_eq!(TriggerType::try_from(n), Ok(t));
        }
        assert!(TriggerType::try_from(99).is_err());
    }

    #[test]
    fn reporter_pull_request_require_approval_defaults_true_for_legacy_rows() {
        // Pre-#247 rows lack `require_approval` in the stored JSON. The serde
        // default must produce `true` on read so secure-by-default applies
        // without a backfill migration on existing trigger rows.
        let legacy_db = serde_json::json!({
            "integration_id": IntegrationId::nil(),
            "branches": [],
            "actions": ["opened"],
        });
        let parsed = TriggerConfig::parse_row(2, &legacy_db).unwrap();
        let TriggerConfig::ReporterPullRequest {
            require_approval, ..
        } = parsed
        else {
            panic!("expected ReporterPullRequest");
        };
        assert!(require_approval, "missing field must default to true");
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
        assert!(
            db.get("type").is_none(),
            "db json should not carry the type tag"
        );
        let parsed = TriggerConfig::parse_row(1, &db).unwrap();
        assert_eq!(parsed, cfg);
    }
}
