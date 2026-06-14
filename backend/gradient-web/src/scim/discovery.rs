/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::error::SCIM_CONTENT_TYPE;

fn scim(body: serde_json::Value) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, SCIM_CONTENT_TYPE)], axum::Json(body)).into_response()
}

pub async fn service_provider_config() -> impl IntoResponse {
    scim(json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],
        "patch": {"supported": true},
        "bulk": {"supported": false, "maxOperations": 0, "maxPayloadSize": 0},
        "filter": {"supported": true, "maxResults": 200},
        "changePassword": {"supported": false},
        "sort": {"supported": false},
        "etag": {"supported": false},
        "authenticationSchemes": [{
            "type": "oauthbearertoken",
            "name": "OAuth Bearer Token",
            "description": "Authentication via the SCIM provisioning bearer token"
        }]
    }))
}

pub async fn resource_types() -> impl IntoResponse {
    scim(json!([
        {"schemas":["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],"id":"User","name":"User","endpoint":"/Users","schema":"urn:ietf:params:scim:schemas:core:2.0:User"},
        {"schemas":["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],"id":"Group","name":"Group","endpoint":"/Groups","schema":"urn:ietf:params:scim:schemas:core:2.0:Group"}
    ]))
}

pub async fn schemas() -> impl IntoResponse {
    scim(json!([
        {"id":"urn:ietf:params:scim:schemas:core:2.0:User","name":"User"},
        {"id":"urn:ietf:params:scim:schemas:core:2.0:Group","name":"Group"}
    ]))
}
