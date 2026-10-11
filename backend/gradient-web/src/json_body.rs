/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebError;
use axum::Json;
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;

pub struct JsonBody<T>(pub T);

impl<T, S> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = WebError;

    async fn from_request(request: Request, state: &S) -> Result<Self, WebError> {
        let Json(value) = Json::<T>::from_request(request, state).await?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::names::{ProjectDisplayName, ProjectName, UserDisplayName};
    use axum::body::Body;
    use axum::http::header::CONTENT_TYPE;
    use serde::Deserialize;

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
}
