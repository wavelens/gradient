/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{ErrorCode, WebError};
use crate::resource::Resource;
use gradient_types::input::{InputError, check_index_name, check_task_name, validate_display_name};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;

pub trait NameKind {
    const RESOURCE: Resource;
    const DISPLAY_LABEL: &'static str = "display name";

    fn check_index(name: &str) -> Result<(), InputError> {
        check_index_name(name)
    }
}

macro_rules! name_kind {
    ($kind:ident, $resource:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $kind;

        impl NameKind for $kind {
            const RESOURCE: Resource = Resource::$resource;
        }
    };
}

name_kind!(ProjectKind, ProjectName);
name_kind!(CacheKind, CacheName);
name_kind!(TeamKind, TeamName);
name_kind!(RoleKind, RoleName);
name_kind!(IntegrationKind, IntegrationName);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskKind;

impl NameKind for TaskKind {
    const RESOURCE: Resource = Resource::TaskName;

    fn check_index(name: &str) -> Result<(), InputError> {
        check_task_name(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserKind;

impl NameKind for UserKind {
    const RESOURCE: Resource = Resource::User;
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
    pub(crate) fn find_in(text: &str) -> Option<Self> {
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

impl From<NameError> for WebError {
    fn from(err: NameError) -> Self {
        match err {
            NameError::Index { subject } => {
                Self::BadRequest(ErrorCode::INVALID_NAME, format!("Invalid {subject}"))
            }
            NameError::Display { label, reason } => {
                Self::BadRequest(ErrorCode::BAD_REQUEST, format!("Invalid {label}: {reason}"))
            }
        }
    }
}

fn check_index<K: NameKind>(name: &str) -> Result<(), NameError> {
    K::check_index(name).map_err(|_| NameError::Index {
        subject: K::RESOURCE.to_string(),
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
pub type IntegrationName = IndexName<IntegrationKind>;

pub type ProjectDisplayName = DisplayName<ProjectKind>;
pub type CacheDisplayName = DisplayName<CacheKind>;
pub type TaskDisplayName = DisplayName<TaskKind>;
pub type TeamDisplayName = DisplayName<TeamKind>;
pub type UserDisplayName = DisplayName<UserKind>;
