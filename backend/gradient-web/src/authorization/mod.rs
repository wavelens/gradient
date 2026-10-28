/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod api_key;
mod job_scope;
mod jwt;
mod middleware;
mod oidc;
mod scim;

pub use self::api_key::{ApiKeyContext, DecodedRequest, JobPrincipal, MaybeApiKey, Principal};
pub use self::job_scope::{JobScopeRefusal, job_mask};
pub use self::jwt::{
    Cliams, DownloadClaims, cookie_value, create_session_and_token, decode_download_token,
    decode_jwt, encode_download_token, generate_api_key, hash_api_key, session_claims,
};
pub use self::middleware::{MaybeUser, authorize, authorize_optional, update_last_login};
pub use self::oidc::{OidcAuthRequest, oidc_login_create, oidc_login_verify};
pub use self::scim::authorize_scim;
