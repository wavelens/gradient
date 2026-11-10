/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::{
    invocation_token_is_from_env_var, saved_login_for_shell_completion,
    update_or_create_saved_login_in_config_file,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use connector::Client;
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct UnverifiedSessionTokenTimestamps {
    iat: u64,
    exp: u64,
}

impl UnverifiedSessionTokenTimestamps {
    fn half_lifetime_passed_at(&self, now: u64) -> bool {
        now >= self.iat + self.exp.saturating_sub(self.iat) / 2 && now < self.exp
    }
}

fn read_session_jwt_issued_at_and_expiry(token: &str) -> Option<UnverifiedSessionTokenTimestamps> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}

pub async fn renew_saved_session_token_if_half_lifetime_passed() {
    if invocation_token_is_from_env_var() {
        return;
    }
    let login = saved_login_for_shell_completion();
    let (Some(server), Some(token)) = (login.server(), login.token()) else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let due = read_session_jwt_issued_at_and_expiry(&token)
        .is_some_and(|timestamps| timestamps.half_lifetime_passed_at(now));
    if !due {
        return;
    }

    let Ok(client) = Client::builder()
        .base_url(server)
        .token(token.clone())
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return;
    };
    let Ok(renewed) = client.auth().renew_session().await else {
        return;
    };

    update_or_create_saved_login_in_config_file(|saved| {
        if saved.auth_token.as_deref() == Some(token.as_str()) {
            saved.auth_token = Some(renewed);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_with_payload(payload: &str) -> String {
        format!("header.{}.signature", URL_SAFE_NO_PAD.encode(payload))
    }

    #[test]
    fn reads_timestamps_from_session_jwt_payload() {
        let token = token_with_payload(r#"{"exp":200,"iat":100,"id":"u","jti":"s"}"#);
        assert_eq!(
            read_session_jwt_issued_at_and_expiry(&token),
            Some(UnverifiedSessionTokenTimestamps { iat: 100, exp: 200 })
        );
    }

    #[test]
    fn api_keys_and_malformed_tokens_have_no_timestamps() {
        for token in [
            "GRADabcdef",
            "a.b.c",
            "only-one-part",
            &token_with_payload("{}"),
        ] {
            assert_eq!(
                read_session_jwt_issued_at_and_expiry(token),
                None,
                "{token}"
            );
        }
    }

    #[test]
    fn renewal_is_due_from_half_lifetime_until_expiry() {
        let timestamps = UnverifiedSessionTokenTimestamps { iat: 100, exp: 200 };
        for (now, due) in [
            (120, false),
            (149, false),
            (150, true),
            (199, true),
            (200, false),
        ] {
            assert_eq!(timestamps.half_lifetime_passed_at(now), due, "now = {now}");
        }
    }
}
