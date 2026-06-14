/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod api_key;
mod jwt;
mod middleware;
mod oidc;
mod scim;

pub use self::api_key::{ApiKeyContext, DecodedRequest, MaybeApiKey};
pub use self::jwt::{
    Cliams, DownloadClaims, create_session_and_token, decode_download_token, decode_jwt,
    encode_download_token, extract_bearer_or_cookie, generate_api_key, hash_api_key,
};
pub use self::middleware::{MaybeUser, authorize, authorize_optional, update_last_login};
pub use self::oidc::{OidcAuthRequest, oidc_login_create, oidc_login_verify};
pub use self::scim::authorize_scim;
