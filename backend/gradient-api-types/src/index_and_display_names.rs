/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

pub const MAX_SLUG_NAME_CHARACTER_COUNT: usize = 64;
pub const RESERVED_TASK_NAMES: &[&str] = &["build-request"];

#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone, Copy)]
pub enum IndexOrDisplayNameValidationError {
    #[error("Name cannot be empty")]
    NameEmpty,
    #[error("Name cannot exceed {max} characters", max = MAX_SLUG_NAME_CHARACTER_COUNT)]
    SlugNameTooLong,
    #[error("Name must be lowercase")]
    NameNotLowercase,
    #[error("Name can only contain letters, numbers and dashes")]
    NameInvalidCharacters,
    #[error("Name can only start and end with letters or numbers")]
    NameInvalidStartEnd,
    #[error("This task name is reserved and cannot be used")]
    TaskNameReserved,
    #[error("Display name cannot be empty")]
    DisplayNameEmpty,
    #[error("Display name cannot exceed 100 characters")]
    DisplayNameTooLong,
    #[error(
        "Display name can only contain letters, numbers, spaces, apostrophes, dots, dashes and underscores"
    )]
    DisplayNameInvalidCharacters,
    #[error("Display name cannot start or end with spaces")]
    DisplayNameInvalidStartEnd,
    #[error("Display name cannot contain consecutive spaces")]
    DisplayNameConsecutiveSpaces,
}

pub fn check_index_name(s: &str) -> Result<(), IndexOrDisplayNameValidationError> {
    use IndexOrDisplayNameValidationError as E;
    if s.is_empty() {
        return Err(E::NameEmpty);
    }

    if s.chars().count() > MAX_SLUG_NAME_CHARACTER_COUNT {
        return Err(E::SlugNameTooLong);
    }

    if s != s.to_lowercase() {
        return Err(E::NameNotLowercase);
    }

    if s.contains(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        return Err(E::NameInvalidCharacters);
    }

    if s.starts_with('-') || s.ends_with('-') {
        return Err(E::NameInvalidStartEnd);
    }

    Ok(())
}

pub fn check_task_name(s: &str) -> Result<(), IndexOrDisplayNameValidationError> {
    check_index_name(s)?;
    if RESERVED_TASK_NAMES
        .iter()
        .any(|r| s.eq_ignore_ascii_case(r))
    {
        return Err(IndexOrDisplayNameValidationError::TaskNameReserved);
    }
    Ok(())
}

pub fn validate_display_name(display_name: &str) -> Result<(), IndexOrDisplayNameValidationError> {
    use IndexOrDisplayNameValidationError as E;
    if display_name.is_empty() {
        return Err(E::DisplayNameEmpty);
    }

    if display_name.len() > 100 {
        return Err(E::DisplayNameTooLong);
    }

    if !display_name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '\'' | '.' | '-' | '_'))
    {
        return Err(E::DisplayNameInvalidCharacters);
    }

    if display_name.starts_with(' ') || display_name.ends_with(' ') {
        return Err(E::DisplayNameInvalidStartEnd);
    }

    if display_name.contains("  ") {
        return Err(E::DisplayNameConsecutiveSpaces);
    }

    Ok(())
}

pub trait NameKind {
    const INVALID_NAME_ERROR_LABEL: &'static str;
    const DISPLAY_LABEL: &'static str = "display name";

    fn check_index(name: &str) -> Result<(), IndexOrDisplayNameValidationError> {
        check_index_name(name)
    }
}

macro_rules! name_kind {
    ($kind:ident, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $kind;

        impl NameKind for $kind {
            const INVALID_NAME_ERROR_LABEL: &'static str = $label;
        }
    };
}

name_kind!(ProjectKind, "Project Name");
name_kind!(CacheKind, "Cache Name");
name_kind!(TeamKind, "Team Name");
name_kind!(RoleKind, "Role Name");
name_kind!(GitHostConnectionKind, "Git host connection name");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskKind;

impl NameKind for TaskKind {
    const INVALID_NAME_ERROR_LABEL: &'static str = "Task Name";

    fn check_index(name: &str) -> Result<(), IndexOrDisplayNameValidationError> {
        check_task_name(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserKind;

impl NameKind for UserKind {
    const INVALID_NAME_ERROR_LABEL: &'static str = "User";
    const DISPLAY_LABEL: &'static str = "name";
}

const INDEX_MARKER: &str = "invalid index name [";
const DISPLAY_MARKER: &str = "invalid display name [";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Index { subject: String },
    Display { label: String, reason: String },
}

impl NameError {
    pub fn find_in(text: &str) -> Option<Self> {
        if let Some((_, rest)) = text.split_once(INDEX_MARKER) {
            let (subject, _) = rest.split_once(']')?;
            return Some(Self::Index {
                subject: subject.to_string(),
            });
        }
        let (_, rest) = text.split_once(DISPLAY_MARKER)?;
        let (inner, _) = rest.split_once(']')?;
        let (label, reason) = inner.split_once(": ")?;
        Some(Self::Display {
            label: label.to_string(),
            reason: reason.to_string(),
        })
    }
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index { subject } => write!(f, "{INDEX_MARKER}{subject}]"),
            Self::Display { label, reason } => write!(f, "{DISPLAY_MARKER}{label}: {reason}]"),
        }
    }
}

impl std::error::Error for NameError {}

fn check_index<K: NameKind>(name: &str) -> Result<(), NameError> {
    K::check_index(name).map_err(|_| NameError::Index {
        subject: K::INVALID_NAME_ERROR_LABEL.to_string(),
    })
}

fn check_display<K: NameKind>(name: &str) -> Result<(), NameError> {
    validate_display_name(name).map_err(|e| NameError::Display {
        label: K::DISPLAY_LABEL.to_string(),
        reason: e.to_string(),
    })
}

macro_rules! checked_name {
    ($type:ident, $check:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $type<K: NameKind>(String, PhantomData<K>);

        impl<K: NameKind> $type<K> {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl<K: NameKind> TryFrom<String> for $type<K> {
            type Error = NameError;

            fn try_from(value: String) -> Result<Self, NameError> {
                $check::<K>(&value)?;
                Ok(Self(value, PhantomData))
            }
        }

        impl<K: NameKind> Deref for $type<K> {
            type Target = str;

            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl<K: NameKind> AsRef<str> for $type<K> {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl<K: NameKind> fmt::Display for $type<K> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<K: NameKind> PartialSchema for $type<K> {
            fn schema() -> RefOr<Schema> {
                String::schema()
            }
        }

        impl<K: NameKind> ToSchema for $type<K> {}

        impl<K: NameKind> Serialize for $type<K> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de, K: NameKind> Deserialize<'de> for $type<K> {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::try_from(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

checked_name!(IndexName, check_index);
checked_name!(DisplayName, check_display);

pub type ProjectName = IndexName<ProjectKind>;
pub type CacheName = IndexName<CacheKind>;
pub type TaskName = IndexName<TaskKind>;
pub type TeamName = IndexName<TeamKind>;
pub type RoleName = IndexName<RoleKind>;
pub type GitHostConnectionName = IndexName<GitHostConnectionKind>;

pub type ProjectDisplayName = DisplayName<ProjectKind>;
pub type CacheDisplayName = DisplayName<CacheKind>;
pub type TaskDisplayName = DisplayName<TaskKind>;
pub type TeamDisplayName = DisplayName<TeamKind>;
pub type UserDisplayName = DisplayName<UserKind>;
