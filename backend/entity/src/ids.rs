/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed newtype wrappers around `Uuid` for every entity primary key.
//!
//! These exist so the compiler can reject argument swaps such as
//! `user_is_org_member(state, org_id, user_id)`. Wire format is unchanged via
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
id_newtype!(BuildProductId);
id_newtype!(BuildRequestBlobId);
id_newtype!(CacheId);
id_newtype!(CacheDerivationId);
id_newtype!(CacheMetricId);
id_newtype!(CacheUpstreamId);
id_newtype!(CacheUserId);
id_newtype!(CachedPathId);
id_newtype!(CachedPathSignatureId);
id_newtype!(CommitId);
id_newtype!(DerivationId);
id_newtype!(DerivationDependencyId);
id_newtype!(DerivationFeatureId);
id_newtype!(DerivationOutputId);
id_newtype!(DerivationOutputSignatureId);
id_newtype!(EntryPointId);
id_newtype!(EntryPointMessageId);
id_newtype!(EvaluationId);
id_newtype!(EvaluationFlakeInputOverrideId);
id_newtype!(EvaluationMessageId);
id_newtype!(FeatureId);
id_newtype!(FlakeInputOverrideId);
id_newtype!(IntegrationId);
id_newtype!(OrganizationId);
id_newtype!(OrganizationCacheId);
id_newtype!(OrganizationUserId);
id_newtype!(ProjectId);
id_newtype!(ProjectActionId);
id_newtype!(ProjectActionDeliveryId);
id_newtype!(ProjectTriggerId);
id_newtype!(RoleId);
id_newtype!(UserId);
id_newtype!(SessionId);
id_newtype!(UploadSessionId);
id_newtype!(AuditLogId);
id_newtype!(WorkerRegistrationId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_uuid_user_id() {
        let u = Uuid::now_v7();
        assert_eq!(Uuid::from(UserId::from(u)), u);
        assert_eq!(UserId::from(u).into_inner(), u);
    }

    #[test]
    fn serde_is_transparent() {
        let u = Uuid::now_v7();
        let typed = serde_json::to_string(&UserId(u)).unwrap();
        let raw = serde_json::to_string(&u).unwrap();
        assert_eq!(typed, raw, "wire format must equal bare Uuid");
    }

    #[test]
    fn serde_deserialize_from_uuid_string() {
        let u = Uuid::now_v7();
        let s = format!("\"{u}\"");
        let id: UserId = serde_json::from_str(&s).unwrap();
        assert_eq!(id.into_inner(), u);
    }

    #[test]
    fn from_str_parses_uuid() {
        let u = Uuid::now_v7();
        let id: UserId = u.to_string().parse().unwrap();
        assert_eq!(id.into_inner(), u);
        assert!("not-a-uuid".parse::<UserId>().is_err());
    }

    #[test]
    fn display_matches_uuid() {
        let u = Uuid::now_v7();
        assert_eq!(format!("{}", UserId::from(u)), u.to_string());
    }

    #[test]
    fn debug_includes_type_name() {
        let u = Uuid::nil();
        let s = format!("{:?}", UserId::from(u));
        assert!(s.starts_with("UserId("), "got: {s}");
    }

    #[test]
    fn try_from_u64_returns_error_for_uuid_pk() {
        assert!(<UserId as TryFromU64>::try_from_u64(0).is_err());
        assert!(<OrganizationId as TryFromU64>::try_from_u64(42).is_err());
    }

    #[test]
    fn distinct_types_compile_to_different_types() {
        let u = Uuid::now_v7();
        let user: UserId = u.into();
        let _org: OrganizationId = user.into_inner().into();
    }
}
