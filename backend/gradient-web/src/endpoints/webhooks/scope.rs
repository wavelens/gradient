/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::WebhookRefusal;
use crate::access::{CacheAccess, Caller, ProjectAccess, load_cache, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, require_superuser};
use crate::permissions::{CachePermission, Permission};
use crate::scope::PathParams;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use gradient_core::ServerState;
use gradient_entity::webhook::WebhookScope;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{ColumnTrait, Condition};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct WebhookOwner {
    pub scope: WebhookScope,
    pub project: Option<ProjectId>,
    pub cache: Option<CacheId>,
    pub user: MUser,
    pub params: PathParams,
}

#[derive(Clone, Copy, Debug)]
pub enum Verb {
    Create,
    Update,
    Delete,
}

impl WebhookOwner {
    pub fn event_owner(&self) -> EventOwner {
        EventOwner {
            project: self.project,
            task: None,
            cache: self.cache,
        }
    }

    pub fn action(&self, verb: Verb) -> Action {
        match (self.scope, verb) {
            (WebhookScope::Project, Verb::Create) => Action::ProjectWebhookCreate,
            (WebhookScope::Project, Verb::Update) => Action::ProjectWebhookUpdate,
            (WebhookScope::Project, Verb::Delete) => Action::ProjectWebhookDelete,
            (WebhookScope::Cache, Verb::Create) => Action::CacheWebhookCreate,
            (WebhookScope::Cache, Verb::Update) => Action::CacheWebhookUpdate,
            (WebhookScope::Cache, Verb::Delete) => Action::CacheWebhookDelete,
            (WebhookScope::Instance, Verb::Create) => Action::InstanceWebhookCreate,
            (WebhookScope::Instance, Verb::Update) => Action::InstanceWebhookUpdate,
            (WebhookScope::Instance, Verb::Delete) => Action::InstanceWebhookDelete,
        }
    }

    pub fn rows(&self) -> Condition {
        Condition::all()
            .add(CWebhook::Scope.eq(self.scope))
            .add(match self.project {
                Some(p) => CWebhook::Project.eq(p),
                None => CWebhook::Project.is_null(),
            })
            .add(match self.cache {
                Some(c) => CWebhook::Cache.eq(c),
                None => CWebhook::Cache.is_null(),
            })
    }

    pub fn param<T: std::str::FromStr>(&self, name: &str) -> Result<T, WebError> {
        self.params.param(name)
    }
}

impl FromRequestParts<Arc<ServerState>> for WebhookOwner {
    type Rejection = WebError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<ServerState>,
    ) -> Result<Self, WebError> {
        let db = state.db();
        let user = parts
            .extensions
            .get::<MUser>()
            .cloned()
            .ok_or(WebhookRefusal::LoginRequired)?;
        let api_key = parts
            .extensions
            .get::<MaybeApiKey>()
            .and_then(|k| k.0.clone());
        let params = PathParams::from_parts(parts, state).await?;

        if params.contains("project") {
            let project = load_project(
                &db,
                Caller::User(&user),
                api_key.as_ref(),
                params.param("project")?,
                ProjectAccess::Require {
                    permission: Permission::ManageWebhooks,
                    reject_managed: false,
                },
            )
            .await?;
            return Ok(Self {
                scope: WebhookScope::Project,
                project: Some(project.id),
                cache: None,
                user,
                params,
            });
        }
        if params.contains("cache") {
            let cache = load_cache(
                &db,
                Caller::User(&user),
                api_key.as_ref(),
                params.param("cache")?,
                CacheAccess::Require {
                    permission: CachePermission::ManageCacheWebhooks,
                    reject_managed: false,
                },
            )
            .await?;
            return Ok(Self {
                scope: WebhookScope::Cache,
                project: None,
                cache: Some(cache.id),
                user,
                params,
            });
        }
        require_superuser(&user)?;
        Ok(Self {
            scope: WebhookScope::Instance,
            project: None,
            cache: None,
            user,
            params,
        })
    }
}
