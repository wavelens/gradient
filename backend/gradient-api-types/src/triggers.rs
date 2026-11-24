/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[repr(i16)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
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
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Polling = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    ReporterPush = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    ReporterPullRequest = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Time = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    PostRequest = 4,
}

pub const MAX_TRIGGER_PATH_FILTER_PATTERN_COUNT: usize = 64;

pub const MAX_TRIGGER_PATH_FILTER_PATTERN_CHARACTER_COUNT: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerConfig {
    Polling {
        interval_secs: u32,
        #[serde(default)]
        branch: Option<String>,
        #[serde(flatten)]
        path_filter: ChangedFilePathFilter,
    },
    ReporterPush {
        #[serde(default)]
        branches: Vec<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        releases_only: bool,
        #[serde(default)]
        include_flake_inputs: bool,
        #[serde(flatten)]
        path_filter: ChangedFilePathFilter,
    },
    ReporterPullRequest {
        #[serde(default)]
        branches: Vec<String>,
        #[serde(default = "default_pr_actions")]
        actions: Vec<String>,
        #[serde(default = "default_require_approval")]
        require_approval: bool,
        #[serde(flatten)]
        path_filter: ChangedFilePathFilter,
    },
    Time {
        cron: String,
        #[serde(default)]
        branch: Option<String>,
    },
    PostRequest {
        #[serde(default)]
        branch: Option<String>,
        #[serde(flatten)]
        path_filter: ChangedFilePathFilter,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct ChangedFilePathFilter {
    pub include_paths: Vec<String>,
    pub exclude_paths: Vec<String>,
}

impl ChangedFilePathFilter {
    pub fn validate(&self) -> Result<(), TriggerConfigError> {
        for pattern in self.include_paths.iter().chain(&self.exclude_paths) {
            if pattern.is_empty() {
                return Err(TriggerConfigError::EmptyPathFilterPattern);
            }
            if pattern.starts_with('/') {
                return Err(TriggerConfigError::PathFilterNotRelative(pattern.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TriggerConfigError {
    #[error("polling interval_secs must be at least 10")]
    PollingIntervalTooSmall,
    #[error("invalid cron expression: {0}")]
    InvalidCron(String),
    #[error("patterns of a path filter can't be empty")]
    EmptyPathFilterPattern,
    #[error("The pattern '{0}' for the path filter needs to be relative to the repository root")]
    PathFilterNotRelative(String),
    #[error(
        "a path filter can have at most {max_count} patterns in total across include and exclude patterns"
    )]
    PathFilterHasTooManyPatterns { max_count: usize },
    #[error("pattern for a path filter can't be longer than {max_length} characters")]
    PathFilterPatternTooLong { max_length: usize },
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
            Self::PostRequest { .. } => TriggerType::PostRequest,
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
        parsed.validate_and_convert_cron_to_six_fields()
    }

    pub fn validate(&self) -> Result<(), TriggerConfigError> {
        match self {
            Self::Polling { interval_secs, .. } if *interval_secs < 10 => {
                return Err(TriggerConfigError::PollingIntervalTooSmall);
            }
            Self::Time { cron, .. } => {
                cron.parse::<cron::Schedule>()
                    .map_err(|e| TriggerConfigError::InvalidCron(e.to_string()))?;
            }
            _ => {}
        }
        self.path_filter()
            .map_or(Ok(()), ChangedFilePathFilter::validate)
    }

    /// The checks cover new input only. Stored triggers above the limits still work and a later load doesn't reject them.
    pub fn validate_path_filter_pattern_limits(&self) -> Result<(), TriggerConfigError> {
        let Some(path_filter) = self.path_filter() else {
            return Ok(());
        };
        if path_filter.include_paths.len() + path_filter.exclude_paths.len()
            > MAX_TRIGGER_PATH_FILTER_PATTERN_COUNT
        {
            return Err(TriggerConfigError::PathFilterHasTooManyPatterns {
                max_count: MAX_TRIGGER_PATH_FILTER_PATTERN_COUNT,
            });
        }
        if path_filter
            .include_paths
            .iter()
            .chain(&path_filter.exclude_paths)
            .any(|pattern| {
                pattern.chars().count() > MAX_TRIGGER_PATH_FILTER_PATTERN_CHARACTER_COUNT
            })
        {
            return Err(TriggerConfigError::PathFilterPatternTooLong {
                max_length: MAX_TRIGGER_PATH_FILTER_PATTERN_CHARACTER_COUNT,
            });
        }
        Ok(())
    }

    pub fn path_filter(&self) -> Option<&ChangedFilePathFilter> {
        match self {
            Self::Polling { path_filter, .. }
            | Self::ReporterPush { path_filter, .. }
            | Self::ReporterPullRequest { path_filter, .. }
            | Self::PostRequest { path_filter, .. } => Some(path_filter),
            Self::Time { .. } => None,
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
            Self::Polling { branch, .. }
            | Self::Time { branch, .. }
            | Self::PostRequest { branch, .. } => branch.as_deref(),
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
            Self::Polling {
                interval_secs,
                path_filter,
                ..
            } => Self::Polling {
                interval_secs,
                branch: Some(branch),
                path_filter,
            },
            Self::Time { cron, .. } => Self::Time {
                cron,
                branch: Some(branch),
            },
            Self::PostRequest { path_filter, .. } => Self::PostRequest {
                branch: Some(branch),
                path_filter,
            },
            Self::ReporterPush {
                tags,
                releases_only,
                include_flake_inputs,
                path_filter,
                ..
            } => Self::ReporterPush {
                branches: vec![branch],
                tags,
                releases_only,
                include_flake_inputs,
                path_filter,
            },
            Self::ReporterPullRequest {
                actions,
                require_approval,
                path_filter,
                ..
            } => Self::ReporterPullRequest {
                branches: vec![branch],
                actions,
                require_approval,
                path_filter,
            },
        }
    }

    pub fn branch_names_or_patterns_for_task_header(&self) -> Vec<Option<String>> {
        match self {
            Self::Polling { branch, .. }
            | Self::Time { branch, .. }
            | Self::PostRequest { branch, .. } => vec![branch.clone()],
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

fn default_pr_actions() -> Vec<String> {
    vec!["opened".into(), "synchronize".into(), "reopened".into()]
}

fn default_require_approval() -> bool {
    true
}
