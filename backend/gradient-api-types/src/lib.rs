/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod ad_hoc_build_request_api_types;
pub mod admin;
pub mod builds;
pub mod caches;
pub mod dashboard_board_api_types;
pub mod evals;
pub mod events;
pub mod exec_jobs;
pub mod git_host_connections;
pub mod global_search_api_types;
pub mod ids;
pub mod index_and_display_names;
pub mod member_role_and_invitation_rest_types;
pub mod membership_invitations;
pub mod permissions;
pub mod projects;
pub mod resource_access;
pub mod server;
pub mod tasks;
pub mod teams;
pub mod transition;
pub mod triggers;
pub mod user_account;
pub mod user_authentication_api_types;
pub mod waiting_reason;
pub mod webhooks;
pub mod workers;

pub use actions::*;
pub use ad_hoc_build_request_api_types::*;
pub use admin::*;
pub use builds::*;
pub use caches::*;
pub use dashboard_board_api_types::*;
pub use evals::*;
pub use events::*;
pub use exec_jobs::*;
pub use git_host_connections::*;
pub use global_search_api_types::*;
pub use ids::*;
pub use index_and_display_names::*;
pub use member_role_and_invitation_rest_types::*;
pub use membership_invitations::*;
pub use permissions::*;
pub use projects::*;
pub use resource_access::*;
pub use server::*;
pub use tasks::*;
pub use teams::*;
pub use transition::*;
pub use triggers::*;
pub use user_account::*;
pub use user_authentication_api_types::*;
pub use waiting_reason::*;
pub use webhooks::*;
pub use workers::*;

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use utoipa::openapi::{ArrayBuilder, ObjectBuilder, Ref, RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

#[derive(Serialize, Deserialize, Debug)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

impl<T: ToSchema> PartialSchema for Paginated<T> {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .property(
                "items",
                ArrayBuilder::new().items(Ref::from_schema_name(T::name())),
            )
            .required("items")
            .property("total", u64::schema())
            .required("total")
            .property("page", u64::schema())
            .required("page")
            .property("per_page", u64::schema())
            .required("per_page")
            .into()
    }
}

impl<T: ToSchema> ToSchema for Paginated<T> {
    fn name() -> Cow<'static, str> {
        Cow::Owned(format!("Paginated_{}", T::name()))
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((T::name().into_owned(), T::schema()));
        T::schemas(schemas);
    }
}

impl<T> Paginated<T> {
    pub fn map<U, F: FnMut(T) -> U>(self, f: F) -> Paginated<U> {
        Paginated {
            items: self.items.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
        }
    }
}

pub fn deserialize_optional_field<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(<Option<T> as serde::Deserialize>::deserialize(de)?))
}

pub(crate) fn default_true() -> bool {
    true
}

macro_rules! impl_display_and_from_str_via_serde {
    ($($enum_type:ty),* $(,)?) => {
        $(
            impl std::fmt::Display for $enum_type {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    match serde_json::to_value(self) {
                        Ok(serde_json::Value::String(text)) => f.write_str(&text),
                        _ => Err(std::fmt::Error),
                    }
                }
            }

            impl std::str::FromStr for $enum_type {
                type Err = serde_json::Error;

                fn from_str(text: &str) -> Result<Self, Self::Err> {
                    serde_json::from_value(serde_json::Value::String(text.to_owned()))
                }
            }
        )*
    };
}

impl_display_and_from_str_via_serde!(
    BuildStatus,
    CacheSubscriptionMode,
    CredentialSchemeItem,
    DeliveryResult,
    EvalCapability,
    EvaluationStatus,
    ExecJobStatus,
    HitKind,
    InviteKind,
    MessageLevel,
    RepositoryGrantSource,
    RequestKind,
    TaskGitHostChainStepState,
    TeamMemberSource,
    TeamRole,
    TriggerType,
);
