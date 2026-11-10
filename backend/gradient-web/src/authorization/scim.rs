/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use gradient_types::input::load_secret;

use super::jwt::bearer_token;
use crate::scim::ScimError;

#[derive(Clone)]
pub struct ScimBearerTokenSha256Digest([u8; 32]);

impl ScimBearerTokenSha256Digest {
    pub fn load(token_file: &str) -> anyhow::Result<Self> {
        let token = load_secret(token_file)?;
        Ok(Self::of(token.expose().as_bytes()))
    }

    fn of(token: &[u8]) -> Self {
        Self(Sha256::digest(token).into())
    }

    fn matches(&self, presented: &[u8]) -> bool {
        Self::of(presented).0.ct_eq(&self.0).into()
    }
}

pub async fn authorize_scim(
    State(expected): State<ScimBearerTokenSha256Digest>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, ScimError> {
    let presented = bearer_token(req.headers())
        .ok_or_else(|| ScimError::unauthorized("missing SCIM bearer token"))?;

    if !expected.matches(presented.as_bytes()) {
        return Err(ScimError::unauthorized("invalid SCIM bearer token"));
    }

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_configured_token_matches() {
        let expected = ScimBearerTokenSha256Digest::of(b"test-scim-token");

        assert!(expected.matches(b"test-scim-token"));
        assert!(!expected.matches(b"test-scim-tokeN"));
        assert!(!expected.matches(b""));
    }
}
