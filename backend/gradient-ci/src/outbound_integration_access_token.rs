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
use sea_orm::ActiveValue::Set;
use sea_orm::{EntityTrait, IntoActiveModel, QuerySelect, TransactionSession, TransactionTrait};

pub fn access_token_expires_at_from_lifetime_seconds(
    now: NaiveDateTime,
    lifetime_seconds: Option<i64>,
) -> Option<NaiveDateTime> {
    lifetime_seconds.map(|seconds| now + Duration::seconds(seconds))
}

fn integration_refreshable_oauth2_token_is_expired_or_expires_within_one_minute(
    integration: &MIntegration,
    now: NaiveDateTime,
) -> bool {
    integration.encrypted_refresh_token.is_some()
        && integration
            .access_token_expires_at
            .is_some_and(|expires_at| expires_at <= now + Duration::minutes(1))
}

fn decrypt_access_token(
    crypt_file: &str,
    integration: &MIntegration,
) -> Result<Option<SecretString>> {
    integration
        .access_token
        .as_deref()
        .map(|encrypted| decrypt_secret_with_file(crypt_file, encrypted))
        .transpose()
        .context("Decrypt the integration access token")
}

pub async fn refresh_and_get_usable_decrypted_access_token(
    db: &impl TransactionTrait,
    config: &RuntimeConfig,
    http: &reqwest::Client,
    integration: &MIntegration,
) -> Result<Option<SecretString>> {
    let crypt_file = config.secrets.crypt_file.as_str();
    let Some(app) = config.gitea_app.as_ref().filter(|_| {
        integration_refreshable_oauth2_token_is_expired_or_expires_within_one_minute(
            integration,
            gradient_types::now(),
        )
    }) else {
        return decrypt_access_token(crypt_file, integration);
    };

    let txn = db.begin().await?;
    let Some(locked) = EIntegration::find_by_id(integration.id)
        .lock_exclusive()
        .one(&txn)
        .await
        .context("Lock Integration for Token Refresh")?
    else {
        return Ok(None);
    };
    let now = gradient_types::now();
    let refresh_token = match locked.encrypted_refresh_token.as_deref() {
        Some(encrypted)
            if integration_refreshable_oauth2_token_is_expired_or_expires_within_one_minute(
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
        .with_context(|| format!("Refresh OAuth2 access token for integration {id}", id = locked.id))?;
    let mut active: AIntegration = locked.into_active_model();
    active.access_token = Set(Some(encrypt_secret_with_file(
        crypt_file,
        &tokens.access_token,
    )?));
    if let Some(rotated) = tokens.refresh_token.as_deref() {
        active.encrypted_refresh_token = Set(Some(encrypt_secret_with_file(crypt_file, rotated)?));
    }
    active.access_token_expires_at = Set(access_token_expires_at_from_lifetime_seconds(
        now,
        tokens.expires_in,
    ));
    gradient_db::rows::update::<EIntegration, _>(&txn, active)
        .await
        .context("Save the updated integration token")?;
    txn.commit().await?;
    Ok(Some(SecretString::from(tokens.access_token)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integration(
        refresh: Option<&str>,
        expires_at: Option<NaiveDateTime>,
    ) -> MIntegration {
        MIntegration {
            id: IntegrationId::now_v7(),
            project: ProjectId::now_v7(),
            name: "forgejo".into(),
            display_name: "Forgejo".into(),
            kind: crate::IntegrationKind::Outbound,
            git_host_type: GitHostType::Forgejo,
            secret: None,
            endpoint_url: Some("https://codeberg.org".into()),
            access_token: Some("encrypted".into()),
            encrypted_refresh_token: refresh.map(str::to_owned),
            access_token_expires_at: expires_at,
            allowed_ips: None,
            github_installation: None,
            created_by: UserId::now_v7(),
            created_at: gradient_types::now(),
            managed: false,
        }
    }

    #[test]
    fn only_expiring_tokens_with_a_refresh_token_need_a_refresh() {
        let now = gradient_types::now();
        let soon = Some(now + Duration::seconds(30));
        let later = Some(now + Duration::hours(1));
        let check = integration_refreshable_oauth2_token_is_expired_or_expires_within_one_minute;

        assert!(check(&integration(Some("r"), soon), now));
        assert!(check(&integration(Some("r"), Some(now - Duration::hours(1))), now));
        assert!(!check(&integration(Some("r"), later), now));
        assert!(!check(&integration(None, soon), now));
        assert!(!check(&integration(Some("r"), None), now));
    }

    #[test]
    fn expiry_adds_the_lifetime_to_now() {
        let now = gradient_types::now();
        assert_eq!(
            access_token_expires_at_from_lifetime_seconds(now, Some(3600)),
            Some(now + Duration::hours(1))
        );
        assert_eq!(access_token_expires_at_from_lifetime_seconds(now, None), None);
    }
}
