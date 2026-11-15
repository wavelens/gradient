/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebError;
use axum::Json;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
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

pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, WebError> {
        let axum::extract::Path(value) =
            axum::extract::Path::<T>::from_request_parts(parts, state).await?;
        Ok(Self(value))
    }
}

pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, WebError> {
        let axum::extract::Query(value) =
            axum::extract::Query::<T>::from_request_parts(parts, state).await?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests;
