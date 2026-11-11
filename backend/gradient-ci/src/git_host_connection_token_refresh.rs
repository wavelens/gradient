/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::{decrypt_secret_with_file, encrypt_secret_with_file};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDateTime};
use gradient_git_host::gitea_oauth::get_new_tokens_from_git_host_with_refresh_token;
use gradient_types::*;
use sea_orm::{EntityTrait, QuerySelect, TransactionSession, TransactionTrait};

pub fn access_token_expires_at_from_lifetime_seconds(
    now: NaiveDateTime,
    lifetime_seconds: Option<i64>,
) -> Option<NaiveDateTime> {
    lifetime_seconds.map(|seconds| now + Duration::seconds(seconds))
}

fn git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute(
    connection: &MGitHostConnection,
    now: NaiveDateTime,
) -> bool {
    connection.encrypted_refresh_token.is_some()
        && connection
            .access_token_expires_at
            .is_some_and(|expires_at| expires_at <= now + Duration::minutes(1))
}

fn decrypt_access_token(
    crypt_file: &str,
    connection: &MGitHostConnection,
) -> Result<Option<SecretString>> {
    connection
        .access_token
        .as_deref()
        .map(|encrypted| decrypt_secret_with_file(crypt_file, encrypted))
        .transpose()
        .context("Decrypt the access token of the Git host connection")
}

pub async fn refresh_and_get_usable_decrypted_access_token(
    db: &impl TransactionTrait,
    config: &RuntimeConfig,
    http: &reqwest::Client,
    connection: &MGitHostConnection,
) -> Result<Option<SecretString>> {
    let crypt_file = config.secrets.crypt_file.as_str();
    let Some(app) = config.gitea_app.as_ref().filter(|_| {
        git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute(
            connection,
            gradient_types::now(),
        )
    }) else {
        return decrypt_access_token(crypt_file, connection);
    };

    let txn = db.begin().await?;
    let Some(locked) = EGitHostConnection::find_by_id(connection.id)
        .lock_exclusive()
        .one(&txn)
        .await
        .context("Lock the connection to the Git host for token refresh.")?
    else {
        return Ok(None);
    };
    let now = gradient_types::now();
    let refresh_token = match locked.encrypted_refresh_token.as_deref() {
        Some(encrypted)
            if git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute(
                &locked, now,
            ) =>
        {
            decrypt_secret_with_file(crypt_file, encrypted)?
        }
        _ => {
            txn.commit().await?;
            return decrypt_access_token(crypt_file, &locked);
        }
    };

    let tokens = get_new_tokens_from_git_host_with_refresh_token(http, app, refresh_token.expose())
        .await
        .with_context(|| {
            format!(
                "Refresh the OAuth2 access token for connection {id} to a Git host",
                id = locked.id
            )
        })?;
    let mut refreshed = locked;
    refreshed.access_token = Some(encrypt_secret_with_file(crypt_file, &tokens.access_token)?);
    if let Some(rotated) = tokens.refresh_token.as_deref() {
        refreshed.encrypted_refresh_token = Some(encrypt_secret_with_file(crypt_file, rotated)?);
    }
    refreshed.access_token_expires_at =
        access_token_expires_at_from_lifetime_seconds(now, tokens.expires_in);
    gradient_db::git_host_connections::write_all_columns_of_git_host_connection(&txn, refreshed)
        .await
        .context("Save the refreshed token for the Git host connection.")?;
    txn.commit().await?;
    Ok(Some(SecretString::from(tokens.access_token)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(refresh: Option<&str>, expires_at: Option<NaiveDateTime>) -> MGitHostConnection {
        MGitHostConnection {
            git_host_type: GitHostType::Forgejo,
            base_url: Some("https://codeberg.org".into()),
            access_token: Some("encrypted".into()),
            encrypted_refresh_token: refresh.map(str::to_owned),
            access_token_expires_at: expires_at,
            ..Default::default()
        }
    }

    #[test]
    fn only_expiring_tokens_with_a_refresh_token_need_a_refresh() {
        let now = gradient_types::now();
        let soon = Some(now + Duration::seconds(30));
        let later = Some(now + Duration::hours(1));
        let check = git_host_connection_has_refresh_token_and_oauth2_access_token_is_expired_or_expires_within_one_minute;

        assert!(check(&connection(Some("r"), soon), now));
        assert!(check(
            &connection(Some("r"), Some(now - Duration::hours(1))),
            now
        ));
        assert!(!check(&connection(Some("r"), later), now));
        assert!(!check(&connection(None, soon), now));
        assert!(!check(&connection(Some("r"), None), now));
    }

    #[test]
    fn expiry_adds_the_lifetime_to_now() {
        let now = gradient_types::now();
        assert_eq!(
            access_token_expires_at_from_lifetime_seconds(now, Some(3600)),
            Some(now + Duration::hours(1))
        );
        assert_eq!(
            access_token_expires_at_from_lifetime_seconds(now, None),
            None
        );
    }
}
