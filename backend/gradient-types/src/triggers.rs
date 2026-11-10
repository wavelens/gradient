/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Six fields, `sec min hour dom mon dow`, are the expected input for the `cron` crate (v0.16).
//! 5-field crontab expressions receive a leading `0` seconds field before saving.

use crate::ids::{IntegrationId, TaskTriggerId};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use gradient_entity::task::ConcurrencyPolicy;
pub use gradient_entity::task_trigger::TriggerType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
        #[serde(default)]
        include_flake_inputs: bool,
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
        #[serde(default)]
        branch: Option<String>,
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
            Self::Time { cron, .. } => {
                cron.parse::<cron::Schedule>()
                    .map_err(|e| TriggerConfigError::InvalidCron(e.to_string()))?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn validate_and_convert_cron_to_six_fields(self) -> Result<Self, TriggerConfigError> {
        let converted = match self {
            Self::Time { cron, branch } if cron.split_whitespace().count() == 5 => Self::Time {
                cron: format!("0 {}", cron.trim()),
                branch,
            },
            other => other,
        };
        converted.validate()?;
        Ok(converted)
    }

    pub fn trigger_branch(&self) -> Option<&str> {
        match self {
            Self::Polling { branch, .. } | Self::Time { branch, .. } => branch.as_deref(),
            Self::ReporterPush { branches, .. } | Self::ReporterPullRequest { branches, .. } => {
                branches
                    .iter()
                    .map(String::as_str)
                    .find(|b| !b.contains(['*', '?', '[']))
            }
        }
    }

    pub fn with_branch(self, branch: String) -> Self {
        match self {
            Self::Polling { interval_secs, .. } => Self::Polling {
                interval_secs,
                branch: Some(branch),
            },
            Self::Time { cron, .. } => Self::Time {
                cron,
                branch: Some(branch),
            },
            Self::ReporterPush {
                integration_id,
                tags,
                releases_only,
                include_flake_inputs,
                ..
            } => Self::ReporterPush {
                integration_id,
                branches: vec![branch],
                tags,
                releases_only,
                include_flake_inputs,
            },
            Self::ReporterPullRequest {
                integration_id,
                actions,
                require_approval,
                ..
            } => Self::ReporterPullRequest {
                integration_id,
                branches: vec![branch],
                actions,
                require_approval,
            },
        }
    }

    pub fn branch_names_or_patterns_for_task_header(&self) -> Vec<Option<String>> {
        match self {
            Self::Polling { branch, .. } | Self::Time { branch, .. } => vec![branch.clone()],
            Self::ReporterPush { branches, .. } | Self::ReporterPullRequest { branches, .. }
                if branches.is_empty() =>
            {
                vec![Some("*".to_owned())]
            }
            Self::ReporterPush { branches, .. } | Self::ReporterPullRequest { branches, .. } => {
                branches.iter().cloned().map(Some).collect()
            }
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

const POLLING_JITTER_PCT: u32 = 10;

pub fn next_poll_time_with_jitter(
    trigger_id: TaskTriggerId,
    last_fired_at: NaiveDateTime,
    interval_secs: u32,
) -> NaiveDateTime {
    let jitter = polling_jitter_secs(trigger_id, last_fired_at, interval_secs);
    last_fired_at + chrono::Duration::seconds(i64::from(interval_secs) + i64::from(jitter))
}

fn polling_jitter_secs(
    trigger_id: TaskTriggerId,
    last_fired_at: NaiveDateTime,
    interval_secs: u32,
) -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let max = interval_secs / (100 / POLLING_JITTER_PCT);
    if max == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    trigger_id.hash(&mut hasher);
    last_fired_at.hash(&mut hasher);
    u32::try_from(hasher.finish() % (u64::from(max) + 1)).unwrap_or(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn polling_jitter_within_ten_percent_bound() {
        let last = dt("2026-05-06 10:00:00");
        let interval = 600;
        let max = interval / 10;
        for _ in 0..1000 {
            let j = polling_jitter_secs(TaskTriggerId::now_v7(), last, interval);
            assert!(j <= max, "jitter {j} exceeded 10% bound {max}");
        }
    }

    #[test]
    fn polling_jitter_is_deterministic_per_cycle() {
        let id = TaskTriggerId::now_v7();
        let last = dt("2026-05-06 10:00:00");
        assert_eq!(
            next_poll_time_with_jitter(id, last, 600),
            next_poll_time_with_jitter(id, last, 600),
            "same (trigger_id, last_fired_at) must yield same jitter"
        );
    }

    #[test]
    fn polling_jitter_changes_between_cycles() {
        let id = TaskTriggerId::now_v7();
        let a = polling_jitter_secs(id, dt("2026-05-06 10:00:00"), 600);
        let b = polling_jitter_secs(id, dt("2026-05-06 10:11:00"), 600);
        let c = polling_jitter_secs(id, dt("2026-05-06 10:22:00"), 600);
        assert!(a != b || a != c, "jitter must vary across cycles");
    }

    #[test]
    fn polling_jitter_zero_when_interval_below_threshold() {
        let last = dt("2026-05-06 10:00:00");
        assert_eq!(
            next_poll_time_with_jitter(TaskTriggerId::now_v7(), last, 9),
            dt("2026-05-06 10:00:09")
        );
    }

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
    fn trigger_branch_skips_glob_patterns() {
        let cfg = TriggerConfig::ReporterPush {
            integration_id: IntegrationId::nil(),
            branches: vec!["release/*".into(), "feat-[ab]".into(), "develop".into()],
            tags: vec![],
            releases_only: false,
            include_flake_inputs: false,
        };
        assert_eq!(cfg.trigger_branch(), Some("develop"));
    }

    #[test]
    fn trigger_branch_without_a_literal_branch_is_the_default_branch() {
        let cfg = TriggerConfig::ReporterPullRequest {
            integration_id: IntegrationId::nil(),
            branches: vec!["*".into()],
            actions: vec![],
            require_approval: true,
        };
        assert_eq!(cfg.trigger_branch(), None);
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
            branch: None,
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
            branch: None,
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn five_field_cron_is_stored_in_the_six_field_form() {
        let cfg = TriggerConfig::Time {
            cron: " 0 2 * * * ".into(),
            branch: Some("release".into()),
        };
        assert_eq!(
            cfg.validate_and_convert_cron_to_six_fields().unwrap(),
            TriggerConfig::Time {
                cron: "0 0 2 * * *".into(),
                branch: Some("release".into()),
            }
        );
    }

    #[test]
    fn six_field_cron_is_kept() {
        let cfg = TriggerConfig::Time {
            cron: "0 30 4 * * Mon".into(),
            branch: None,
        };
        assert_eq!(
            cfg.clone()
                .validate_and_convert_cron_to_six_fields()
                .unwrap(),
            cfg
        );
    }

    #[test]
    fn time_trigger_branch_is_the_trigger_branch() {
        let cfg = TriggerConfig::Time {
            cron: "0 0 2 * * *".into(),
            branch: Some("release".into()),
        };
        assert_eq!(cfg.trigger_branch(), Some("release"));
    }

    #[test]
    fn time_rows_without_branch_read_as_the_default_branch() {
        let legacy_db = serde_json::json!({ "cron": "0 0 2 * * *" });
        let parsed = TriggerConfig::parse_row(TriggerType::Time, &legacy_db).unwrap();
        assert_eq!(parsed.trigger_branch(), None);
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

    #[test]
    fn with_branch_replaces_the_branch_of_every_kind() {
        let integration_id = IntegrationId::nil();
        let release = || "release".to_owned();
        let polling = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
        };
        let time = TriggerConfig::Time {
            cron: "0 0 3 * * *".into(),
            branch: Some("main".into()),
        };
        let push = TriggerConfig::ReporterPush {
            integration_id,
            branches: vec!["main".into(), "dev".into()],
            tags: vec!["v*".into()],
            releases_only: false,
            include_flake_inputs: true,
        };
        for cfg in [polling, time, push] {
            let moved = cfg.with_branch(release());
            assert_eq!(moved.trigger_branch(), Some("release"));
            assert_eq!(
                moved.branch_names_or_patterns_for_task_header(),
                vec![Some(release())]
            );
        }
    }

    #[test]
    fn an_empty_branch_filter_reads_as_any_branch_in_the_header() {
        let push = TriggerConfig::ReporterPullRequest {
            integration_id: IntegrationId::nil(),
            branches: vec![],
            actions: vec![],
            require_approval: true,
        };
        assert_eq!(
            push.branch_names_or_patterns_for_task_header(),
            vec![Some("*".to_owned())]
        );
        let polling = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
        };
        assert_eq!(
            polling.branch_names_or_patterns_for_task_header(),
            vec![None]
        );
    }
}
