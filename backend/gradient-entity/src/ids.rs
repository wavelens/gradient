/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed newtype wrappers around `Uuid` for every entity primary key.
//!
//! These exist so the compiler can reject argument swaps such as
//! `user_is_project_member(state, project_id, user_id)`. Wire format is unchanged via
//! `#[serde(transparent)]`; SeaORM column type is unchanged via
//! `#[derive(DeriveValueType)]`.

use sea_orm::{DbErr, DeriveValueType, TryFromU64};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(
            Copy,
            Clone,
            Default,
            Eq,
            PartialEq,
            Hash,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
            DeriveValueType,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub const fn new(id: Uuid) -> Self {
                Self(id)
            }
            pub const fn into_inner(self) -> Uuid {
                self.0
            }
            pub fn now_v7() -> Self {
                Self(Uuid::now_v7())
            }
            pub const fn nil() -> Self {
                Self(Uuid::nil())
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }
        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<Uuid>().map(Self)
            }
        }
        impl TryFromU64 for $name {
            fn try_from_u64(_: u64) -> Result<Self, DbErr> {
                Err(DbErr::ConvertFromU64(stringify!($name)))
            }
        }
    };
}

id_newtype!(AdminTaskId);
id_newtype!(ApiId);
id_newtype!(BuildId);
id_newtype!(BuildLogChunkId);
id_newtype!(BuildProductId);
id_newtype!(BuildRequestBlobId);
id_newtype!(CacheId);
id_newtype!(CacheInvitationId);
id_newtype!(CacheMetricId);
id_newtype!(CacheSubscriptionRequestId);
id_newtype!(CacheUpstreamId);
id_newtype!(UpstreamMetricId);
id_newtype!(CacheUserId);
id_newtype!(CachedPathId);
id_newtype!(CachedPathSignatureId);
id_newtype!(CommitId);
id_newtype!(DebugInfoId);
id_newtype!(DerivationId);
id_newtype!(DerivationMetricId);
id_newtype!(DerivationFeatureId);
id_newtype!(DerivationOutputId);
id_newtype!(DerivationBuildId);
id_newtype!(DerivationOutputSignatureId);
id_newtype!(EntryPointId);
id_newtype!(EntryPointDepCountId);
id_newtype!(EntryPointMessageId);
id_newtype!(EvalCacheStoreId);
id_newtype!(EvaluationId);
id_newtype!(EvaluationAttrCostId);
id_newtype!(EvaluationMetricId);
id_newtype!(EvaluationFlakeInputOverrideId);
id_newtype!(EvaluationInputUpdateId);
id_newtype!(FlakeOutputNodeId);
id_newtype!(EvaluationMessageId);
id_newtype!(FeatureId);
id_newtype!(FlakeInputOverrideId);
id_newtype!(GithubInstallationId);
id_newtype!(IntegrationId);
id_newtype!(OpenPrStateId);
id_newtype!(OutboxId);
id_newtype!(ProjectId);
id_newtype!(ProjectCacheId);
id_newtype!(ProjectInvitationId);
id_newtype!(ProjectUserId);
id_newtype!(TaskId);
id_newtype!(TaskActionId);
id_newtype!(TaskActionDeliveryId);
id_newtype!(TaskTriggerId);
id_newtype!(RoleId);
id_newtype!(UserId);
id_newtype!(SessionId);
id_newtype!(UploadSessionId);
id_newtype!(AuditLogId);
id_newtype!(WorkerRegistrationId);
id_newtype!(CliDeviceAuthorizationId);
id_newtype!(BuildAttemptId);
id_newtype!(BuildJobId);
id_newtype!(DispatchedJobId);
id_newtype!(DispatchedJobPhaseId);
id_newtype!(MetricRollupId);
id_newtype!(PhaseEventId);
id_newtype!(WorkerConnectionId);
id_newtype!(WorkerSampleId);
id_newtype!(BaseWorkerId);
id_newtype!(ProjectBaseWorkerId);
id_newtype!(WebhookId);
id_newtype!(WebhookDeliveryId);
