/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `/projects/{project}/webhooks`, `/caches/{cache}/webhooks`, `/admin/webhooks`:
//! one handler set, scoped by [`WebhookOwner`].

mod handlers;
mod scope;

pub use scope::{Verb, WebhookOwner};

use axum::Router;
use axum::routing::{get, post};
use gradient_core::ServerState;
use std::sync::Arc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route(
            "/",
            get(handlers::list_webhooks).post(handlers::create_webhook),
        )
        .route(
            "/{id}",
            get(handlers::read_webhook)
                .patch(handlers::update_webhook)
                .delete(handlers::delete_webhook),
        )
        .route("/{id}/test", post(handlers::test_webhook))
        .route("/{id}/rotate-secret", post(handlers::rotate_secret))
        .route("/{id}/deliveries", get(handlers::list_deliveries))
        .route(
            "/{id}/deliveries/{delivery_id}",
            get(handlers::get_delivery),
        )
}
