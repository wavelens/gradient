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
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, SCIM_CONTENT_TYPE)],
        axum::Json(body),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/ServiceProviderConfig",
    operation_id = "scimServiceProviderConfig",
    tag = "scim",
    summary = "SCIM service provider configuration",
    description = "Advertises supported SCIM features (patch, filter, no bulk/sort/etag/change-password).",
    responses((status = OK, description = "Service provider configuration", body = serde_json::Value, content_type = "application/scim+json"), (status = 401, description = "Missing or invalid SCIM bearer token", body = crate::scim::ScimErrorBody, content_type = "application/scim+json"))
)]
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

#[utoipa::path(
    get,
    path = "/ResourceTypes",
    operation_id = "scimResourceTypes",
    tag = "scim",
    summary = "SCIM resource types",
    description = "Lists the supported resource types (User, Group).",
    responses((status = OK, description = "Resource types", body = serde_json::Value, content_type = "application/scim+json"), (status = 401, description = "Missing or invalid SCIM bearer token", body = crate::scim::ScimErrorBody, content_type = "application/scim+json"))
)]
pub async fn resource_types() -> impl IntoResponse {
    scim(json!([
        {"schemas":["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],"id":"User","name":"User","endpoint":"/Users","schema":"urn:ietf:params:scim:schemas:core:2.0:User"},
        {"schemas":["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],"id":"Group","name":"Group","endpoint":"/Groups","schema":"urn:ietf:params:scim:schemas:core:2.0:Group"}
    ]))
}

#[utoipa::path(
    get,
    path = "/Schemas",
    operation_id = "scimSchemas",
    tag = "scim",
    summary = "SCIM schemas",
    description = "Lists the supported core schemas (User, Group).",
    responses((status = OK, description = "Schemas", body = serde_json::Value, content_type = "application/scim+json"), (status = 401, description = "Missing or invalid SCIM bearer token", body = crate::scim::ScimErrorBody, content_type = "application/scim+json"))
)]
pub async fn schemas() -> impl IntoResponse {
    scim(json!([
        {"id":"urn:ietf:params:scim:schemas:core:2.0:User","name":"User"},
        {"id":"urn:ietf:params:scim:schemas:core:2.0:Group","name":"Group"}
    ]))
}
