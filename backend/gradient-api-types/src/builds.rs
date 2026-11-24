/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use crate::*;
use serde::{Deserialize, Serialize};

#[repr(i32)]
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize, utoipa::ToSchema,
)]
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
    sea_orm(rs_type = "i32", db_type = "Integer")
)]
pub enum BuildStatus {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Created = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Queued = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    Building = 2,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Completed = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    FailedPermanent = 4,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 5)
    )]
    Aborted = 5,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 6)
    )]
    DependencyFailed = 6,
    /// `Substituted` is equal to `Completed` at every gate. The outputs were already valid or
    /// fetched by a passthrough from an upstream cache.
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 7)
    )]
    Substituted = 7,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 8)
    )]
    FailedTransient = 8,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 9)
    )]
    FailedTimeout = 9,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 10)
    )]
    Skipped = 10,
}

impl BuildStatus {
    pub const fn for_api(self) -> Self {
        match self {
            Self::Created => Self::Queued,
            other => other,
        }
    }

    pub const fn is_failure(self) -> bool {
        matches!(
            self,
            Self::FailedPermanent
                | Self::FailedTransient
                | Self::FailedTimeout
                | Self::DependencyFailed
        )
    }

    pub const fn is_terminal_failure(self) -> bool {
        matches!(
            self,
            Self::FailedPermanent | Self::FailedTimeout | Self::DependencyFailed
        )
    }

    pub const fn is_terminal_success(self) -> bool {
        matches!(self, Self::Completed | Self::Substituted)
    }

    pub const UNFINISHED: [Self; 5] = [
        Self::Created,
        Self::Queued,
        Self::Building,
        Self::FailedTransient,
        Self::Skipped,
    ];

    pub const TERMINAL: [Self; 6] = [
        Self::Completed,
        Self::Substituted,
        Self::FailedPermanent,
        Self::FailedTimeout,
        Self::Aborted,
        Self::DependencyFailed,
    ];

    pub const fn is_terminal(self) -> bool {
        !matches!(
            self,
            Self::Created | Self::Queued | Self::Building | Self::FailedTransient | Self::Skipped
        )
    }

    pub const fn sources(self) -> &'static [Self] {
        match self {
            Self::Created => &[Self::Skipped, Self::Queued, Self::FailedTransient],
            Self::Queued => &[Self::Created, Self::Building],
            Self::Building => &[Self::Queued],
            Self::Skipped => &[Self::Created],
            Self::Completed
            | Self::Substituted
            | Self::FailedPermanent
            | Self::FailedTransient
            | Self::FailedTimeout
            | Self::Aborted
            | Self::DependencyFailed => &Self::UNFINISHED,
        }
    }

    pub const fn report(self) -> Transition<Self> {
        Transition {
            from: self.sources(),
            to: self,
        }
    }

    pub const PENDING: [Self; 2] = [Self::Created, Self::Queued];

    pub const TERMINAL_SUCCESS: [Self; 2] = [Self::Completed, Self::Substituted];

    pub const TERMINAL_FAILURE: [Self; 3] = [
        Self::FailedPermanent,
        Self::DependencyFailed,
        Self::FailedTimeout,
    ];

    pub const FAILURE: [Self; 4] = [
        Self::FailedPermanent,
        Self::DependencyFailed,
        Self::FailedTransient,
        Self::FailedTimeout,
    ];

    pub const ABORTABLE: [Self; 3] = [Self::Created, Self::Queued, Self::Building];

    pub const RETRYABLE: [Self; 3] = [Self::FailedPermanent, Self::FailedTimeout, Self::Aborted];

    pub const REQUEUEABLE: [Self; 4] = [
        Self::FailedPermanent,
        Self::Aborted,
        Self::DependencyFailed,
        Self::FailedTimeout,
    ];

    pub const QUEUE: Transition<Self> = Transition {
        from: &[Self::Created],
        to: Self::Queued,
    };

    pub const UNQUEUE: Transition<Self> = Transition {
        from: &[Self::Queued],
        to: Self::Created,
    };

    pub const SKIP_UNWANTED: Transition<Self> = Transition {
        from: &[Self::Created],
        to: Self::Skipped,
    };

    pub const THAW_WANTED: Transition<Self> = Transition {
        from: &[Self::Aborted, Self::Skipped],
        to: Self::Created,
    };

    pub const SUBSTITUTE_CREATED: Transition<Self> = Transition {
        from: &[Self::Created],
        to: Self::Substituted,
    };

    pub const COMPLETE_CACHED: Transition<Self> = Transition {
        from: &[
            Self::Created,
            Self::Queued,
            Self::Building,
            Self::FailedPermanent,
            Self::Aborted,
            Self::DependencyFailed,
            Self::FailedTransient,
            Self::FailedTimeout,
            Self::Skipped,
        ],
        to: Self::Completed,
    };

    pub const CASCADE_DEPENDENCY_FAILED: Transition<Self> = Transition {
        from: &[Self::Created, Self::Queued, Self::FailedTransient],
        to: Self::DependencyFailed,
    };

    pub const REQUEUE: Transition<Self> = Transition {
        from: &Self::REQUEUEABLE,
        to: Self::Created,
    };

    pub const RESET_UNCACHED: Transition<Self> = Transition {
        from: &Self::TERMINAL_SUCCESS,
        to: Self::Created,
    };

    pub const REQUEUE_MID_FLIGHT: Transition<Self> = Transition {
        from: &[Self::Building],
        to: Self::Queued,
    };

    pub const ABORT: Transition<Self> = Transition {
        from: &Self::ABORTABLE,
        to: Self::Aborted,
    };

    pub const BUILD_AFTER_SUBSTITUTE_MISSES: Transition<Self> = Transition {
        from: &Self::UNFINISHED,
        to: Self::Created,
    };
}

pub type Architecture = String;

impl From<BuildStatus> for i16 {
    fn from(status: BuildStatus) -> Self {
        status as Self
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct ClosureNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub nar_size: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct ClosureEdge {
    pub source: String,
    pub target: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct ClosureGraph {
    pub roots: Vec<String>,
    pub total_size_bytes: Option<i64>,
    pub node_count: usize,
    pub edge_count: usize,
    pub truncated: bool,
    pub nodes: Vec<ClosureNode>,
    pub edges: Vec<ClosureEdge>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct DependencyGraphNode {
    pub id: DerivationId,
    pub build: Option<BuildJobId>,
    pub name: String,
    pub path: String,
    pub status: Option<String>,
    pub build_time_ms: Option<i64>,
    pub build_started_at: Option<chrono::NaiveDateTime>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct DependencyEdge {
    pub source: DerivationId,
    pub target: DerivationId,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct BuildGraph {
    pub root: DerivationId,
    pub nodes: Vec<DependencyGraphNode>,
    pub edges: Vec<DependencyEdge>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct BuildWithOutputs {
    pub id: BuildJobId,
    pub evaluation: EvaluationId,
    pub status: BuildStatus,
    pub derivation_path: String,
    #[schema(value_type = String)]
    pub architecture: Architecture,
    pub worker: Option<String>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub output: HashMap<String, String>,
    pub prioritized: bool,
    pub ifd: bool,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
    pub progress: Option<BuildProgress>,
    pub entry_points: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, utoipa::ToSchema)]
pub struct BuildProduct {
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
}
