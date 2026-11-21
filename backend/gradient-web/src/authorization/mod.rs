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
    ApiKeyContext, DecodedRequest, JobPrincipal, JobTokenTaskAndExecJobIds, MaybeApiKey, Principal,
    RateLimitBucketOwner,
};
pub use self::external_login::{PasswordLogin, sign_in_external};
pub use self::identity_connect::{ConnectRefusal, can_disconnect, connect_identity};
pub(crate) use self::job_scope::reject_job_token_without_project_credentials_or_privileged_actions;
pub use self::job_scope::{JobScopeRefusal, job_mask};
pub use self::jwt::{
    Cliams, DownloadClaims, clear_jwt_token_cookie, cookie_value,
    create_session_and_token_with_locked_authorization_recheck, decode_download_token, decode_jwt,
    encode_download_token, generate_api_key, hash_api_key, jwt_cookie,
    renew_session_expiry_and_sign_token, session_claims,
};
pub(crate) use self::jwt::{bearer_token, sign_claims, verify_claims};
pub use self::middleware::{
    MaybeUser, authorize, authorize_optional, credential_insert_error_to_http_error,
    move_token_query_parameter_into_authorization_header,
    refuse_cli_session_revoking_other_session, reject_api_key_authentication,
    require_browser_session_authentication, require_session_authentication, update_last_login,
};
pub(crate) use self::oidc::random_url_safe;
pub use self::oidc::{OidcAuthRequest, oidc_login_create, oidc_login_verify};
pub use self::provider_flow::{
    ProviderCallback, ProviderPurpose, provider_authorize, provider_callback,
};
pub use self::scim::{ScimBearerTokenSha256Digest, authorize_scim};
pub(crate) use self::websocket_credential_recheck::AnonymousWebSocketOpenLimitByIpAddress;
pub use self::websocket_credential_recheck::{
    AccessCheckedWebSocketUpgrade, ServeUrlOrigin, WebSocketAuthorizationRecheck,
    WebSocketCredentialRecheckDependencies, WebSocketUpgradeCredentialRecheckRequestExtractor,
    WebsocketCredentialRecheckClosure,
};
