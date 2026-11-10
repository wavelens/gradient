/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod api_key;
mod cache_or_project_restricted_api_key_route_access;
mod external_login;
mod identity_connect;
mod job_scope;
mod jwt;
mod middleware;
mod oidc;
mod provider_flow;
mod scim;
mod websocket_credential_recheck;

pub use self::api_key::{
    ApiKeyContext, DecodedRequest, JobPrincipal, MaybeApiKey, Principal, RateLimitBucketOwner,
};
pub use self::external_login::{PasswordLogin, sign_in_external};
pub use self::identity_connect::{ConnectRefusal, can_disconnect, connect_identity};
pub use self::job_scope::{JobScopeRefusal, job_mask};
pub use self::jwt::{
    Cliams, DownloadClaims, clear_jwt_token_cookie, cookie_value, create_session_and_token,
    decode_download_token, decode_jwt, encode_download_token, generate_api_key, hash_api_key,
    jwt_cookie, session_claims,
};
pub use self::middleware::{
    MaybeUser, authorize, authorize_optional, reject_api_key_authentication,
    require_session_authentication, update_last_login,
};
pub use self::oidc::{OidcAuthRequest, oidc_login_create, oidc_login_verify};
pub use self::provider_flow::{
    ProviderCallback, ProviderPurpose, provider_authorize, provider_callback,
};
pub use self::scim::authorize_scim;
pub use self::websocket_credential_recheck::{
    LiveStreamAuthToken, WebSocketCredentialRecheckDependencies,
};
