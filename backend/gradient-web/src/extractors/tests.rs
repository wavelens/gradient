/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use crate::error::ErrorCode;
use crate::names::{ProjectDisplayName, ProjectName, UserDisplayName};
use axum::body::Body;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::routing::get;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct Sample {
    name: Option<ProjectName>,
    display_name: Option<ProjectDisplayName>,
    person: Option<UserDisplayName>,
}

async fn rejection(body: &str) -> (ErrorCode, String) {
    let request = Request::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    match JsonBody::<Sample>::from_request(request, &()).await {
        Err(WebError::BadRequest(code, message)) => (code, message),
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("body was accepted"),
    }
}

#[tokio::test]
async fn invalid_index_name_is_invalid_name() {
    assert_eq!(
        rejection(r#"{"name": "Not Valid"}"#).await,
        (ErrorCode::INVALID_NAME, "Invalid Project Name".to_string())
    );
}

#[tokio::test]
async fn invalid_display_name_keeps_its_reason() {
    assert_eq!(
        rejection(r#"{"display_name": ""}"#).await,
        (
            ErrorCode::BAD_REQUEST,
            "Invalid display name: Display name cannot be empty".to_string()
        )
    );
    assert_eq!(
        rejection(r#"{"person": " Alice"}"#).await,
        (
            ErrorCode::BAD_REQUEST,
            "Invalid name: Display name cannot start or end with spaces".to_string()
        )
    );
}

#[tokio::test]
async fn other_malformed_json_stays_json_parsing() {
    for body in [r#"{"name": "#, r#"{"name": 5}"#, r#"{"unknown": }"#] {
        let (code, message) = rejection(body).await;
        assert_eq!(code, ErrorCode::JSON_PARSING, "{body}");
        assert!(message.starts_with("Invalid JSON: "), "{message}");
    }
}

#[tokio::test]
async fn valid_names_are_accepted() {
    let request = Request::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"name": "my-project", "display_name": "My Project", "person": "Alice"}"#,
        ))
        .unwrap();
    let Ok(JsonBody(sample)) = JsonBody::<Sample>::from_request(request, &()).await else {
        panic!("valid body was rejected");
    };
    assert_eq!(sample.name.unwrap().as_str(), "my-project");
    assert_eq!(sample.display_name.unwrap().as_str(), "My Project");
    assert_eq!(sample.person.unwrap().as_str(), "Alice");
}

#[tokio::test]
async fn oversized_json_body_stays_payload_too_large() {
    let request = Request::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from("a".repeat(2 * 1024 * 1024 + 1)))
        .unwrap();
    assert!(matches!(
        JsonBody::<Sample>::from_request(request, &()).await,
        Err(WebError::PayloadTooLarge(ErrorCode::PAYLOAD_TOO_LARGE, _))
    ));
}

async fn routed_uuid_path_response(route: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    use tower::ServiceExt;

    let response = axum::Router::new()
        .route(route, get(|Path(_id): Path<uuid::Uuid>| async {}))
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn unparsable_path_segment_is_bad_request() {
    let (status, body) = routed_uuid_path_response("/items/{id}", "/items/not-a-uuid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("Invalid URL: "),
        "{body}"
    );
}

#[tokio::test]
async fn route_without_path_parameters_is_internal_error() {
    let (status, body) = routed_uuid_path_response("/items", "/items").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "internal");
}

#[tokio::test]
async fn unparsable_query_string_is_bad_request() {
    let (mut parts, ()) = Request::get("/items?limit=many")
        .body(())
        .unwrap()
        .into_parts();
    match Query::<HashMap<String, u32>>::from_request_parts(&mut parts, &()).await {
        Err(WebError::BadRequest(code, message)) => {
            assert_eq!(code, ErrorCode::BAD_REQUEST);
            assert!(
                message.starts_with("Failed to deserialize query string: "),
                "{message}"
            );
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("query string was accepted"),
    }
}
