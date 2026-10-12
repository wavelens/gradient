/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod handlers;
mod refusal;
mod responses;
mod scope;

pub use responses::{ActionDeliveryItem, DeliveryDetail, DeliveryListItem, WebhookDeliveryItem};
pub use scope::{Verb, WebhookOwner};

use crate::api_spec::{CachePath, ProjectPath, operations_mut};
use gradient_core::ServerState;
use std::sync::Arc;
use utoipa::IntoParams;
use utoipa::openapi::RefOr;
use utoipa::openapi::path::{Parameter, ParameterIn};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

#[derive(Clone, Copy)]
pub enum WebhookMount {
    Project,
    Cache,
    Instance,
}

impl WebhookMount {
    fn owner(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Cache => "cache",
            Self::Instance => "instance",
        }
    }

    fn requires(self) -> &'static str {
        match self {
            Self::Project => "`manageWebhooks` on the project",
            Self::Cache => "`manageCacheWebhooks` on the cache",
            Self::Instance => "superuser",
        }
    }

    fn parameters(self) -> Vec<Parameter> {
        let path = || Some(ParameterIn::Path);
        match self {
            Self::Project => ProjectPath::into_params(path),
            Self::Cache => CachePath::into_params(path),
            Self::Instance => Vec::new(),
        }
    }

    fn fill(self, text: &str) -> String {
        let owner = self.owner();
        let mut capitalized = owner.to_string();
        capitalized[..1].make_ascii_uppercase();
        text.replace("{owner}", owner)
            .replace("{Owner}", &capitalized)
            .replace("{requires}", self.requires())
    }
}

pub fn router(mount: WebhookMount) -> OpenApiRouter<Arc<ServerState>> {
    let mut router = OpenApiRouter::new()
        .routes(routes!(handlers::list_webhooks, handlers::create_webhook))
        .routes(routes!(
            handlers::read_webhook,
            handlers::update_webhook,
            handlers::delete_webhook
        ))
        .routes(routes!(handlers::test_webhook))
        .routes(routes!(handlers::rotate_secret))
        .routes(routes!(handlers::list_deliveries))
        .routes(routes!(handlers::get_delivery));
    for operation in operations_mut(router.get_openapi_mut()) {
        operation.operation_id = operation.operation_id.take().map(|text| mount.fill(&text));
        operation.summary = operation.summary.take().map(|text| mount.fill(&text));
        operation.description = operation.description.take().map(|text| mount.fill(&text));
        operation
            .parameters
            .get_or_insert_default()
            .extend(mount.parameters().into_iter().map(RefOr::T));
    }
    router
}
