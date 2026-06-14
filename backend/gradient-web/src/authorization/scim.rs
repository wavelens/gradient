/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, header};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

use gradient_core::ServerState;
use gradient_types::input::load_secret;

use crate::scim::ScimError;

/// Validates the SCIM bearer token against the secret in `scim.token_file`.
/// Rejects with a SCIM-shaped 401 on any mismatch. Only mounted when SCIM is
/// configured, so `state.config.scim` is `Some` here.
pub async fn authorize_scim(
    State(state): State<Arc<ServerState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, ScimError> {
    let Some(scim) = state.config.scim.as_ref() else {
        return Err(ScimError::unauthorized("SCIM is not enabled"));
    };

    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .ok_or_else(|| ScimError::unauthorized("missing SCIM bearer token"))?;

    let expected = load_secret(&scim.token_file)
        .map_err(|_| ScimError::internal("SCIM token file unreadable"))?;

    if !constant_time_eq(presented.as_bytes(), expected.expose().as_bytes()) {
        return Err(ScimError::unauthorized("invalid SCIM bearer token"));
    }

    Ok(next.run(req).await)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    Sha256::digest(a) == Sha256::digest(b)
}
