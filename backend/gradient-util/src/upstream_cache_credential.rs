/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpstreamCacheCredentialError {
    #[error("The basic auth user can't be empty.")]
    EmptyBasicAuthUserName,
    #[error("A user name for basic authentication cannot contain `:`.")]
    BasicAuthUserNameContainsColon,
    #[error("The password for basic auth can't be empty.")]
    BasicAuthPasswordIsEmpty,
    #[error("The bearer token can't be empty.")]
    BearerTokenEmptyAfterTrim,
}

pub fn trim_and_validate_basic_auth_user_name(
    user: &str,
) -> Result<&str, UpstreamCacheCredentialError> {
    let user = user.trim();
    if user.is_empty() {
        return Err(UpstreamCacheCredentialError::EmptyBasicAuthUserName);
    }
    if user.contains(':') {
        return Err(UpstreamCacheCredentialError::BasicAuthUserNameContainsColon);
    }
    Ok(user)
}

pub fn validate_basic_auth_and_make_upstream_cache_credential(
    user: &str,
    password: &str,
) -> Result<String, UpstreamCacheCredentialError> {
    let user = trim_and_validate_basic_auth_user_name(user)?;
    if password.is_empty() {
        return Err(UpstreamCacheCredentialError::BasicAuthPasswordIsEmpty);
    }
    Ok(format!("{user}:{password}"))
}

pub fn upstream_cache_credential_plaintext_from_bearer_token(
    token: &str,
) -> Result<String, UpstreamCacheCredentialError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(UpstreamCacheCredentialError::BearerTokenEmptyAfterTrim);
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_validation_trims_basic_auth_user_and_bearer_token_and_reports_empty_part_or_colon_in_user()
     {
        assert_eq!(
            validate_basic_auth_and_make_upstream_cache_credential(" u ", "p w"),
            Ok("u:p w".to_owned())
        );
        assert_eq!(
            upstream_cache_credential_plaintext_from_bearer_token(" t "),
            Ok("t".to_owned())
        );
        assert_eq!(
            validate_basic_auth_and_make_upstream_cache_credential("  ", "p"),
            Err(UpstreamCacheCredentialError::EmptyBasicAuthUserName)
        );
        assert_eq!(
            validate_basic_auth_and_make_upstream_cache_credential("u", ""),
            Err(UpstreamCacheCredentialError::BasicAuthPasswordIsEmpty)
        );
        assert_eq!(
            validate_basic_auth_and_make_upstream_cache_credential("a:b", "p"),
            Err(UpstreamCacheCredentialError::BasicAuthUserNameContainsColon)
        );
        assert_eq!(
            upstream_cache_credential_plaintext_from_bearer_token(" "),
            Err(UpstreamCacheCredentialError::BearerTokenEmptyAfterTrim)
        );
    }
}
