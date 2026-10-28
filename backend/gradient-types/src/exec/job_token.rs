/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{Duration, NaiveDateTime};
use rand::distr::{Alphanumeric, SampleString};
use sha2::{Digest, Sha256};

pub const JOB_TOKEN_PREFIX: &str = "GRJ_";

const JOB_TOKEN_SECRET_LENGTH: usize = 64;
const JOB_TOKEN_GRACE: Duration = Duration::hours(1);
const JOB_TOKEN_WITHOUT_TIMEOUT: Duration = Duration::days(30);

pub fn mint_job_token() -> String {
    let secret = Alphanumeric.sample_string(&mut rand::rng(), JOB_TOKEN_SECRET_LENGTH);
    format!("{JOB_TOKEN_PREFIX}{secret}")
}

pub fn hash_job_token(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

pub fn job_token_expiry(now: NaiveDateTime, timeout_secs: Option<u64>) -> NaiveDateTime {
    let lifetime = match timeout_secs {
        Some(secs) => Duration::try_seconds(i64::try_from(secs).unwrap_or(i64::MAX))
            .and_then(|timeout| timeout.checked_add(&JOB_TOKEN_GRACE))
            .unwrap_or(Duration::MAX),
        None => JOB_TOKEN_WITHOUT_TIMEOUT,
    };
    now.checked_add_signed(lifetime)
        .unwrap_or(NaiveDateTime::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_token_is_stored_only_as_its_hash() {
        let raw = mint_job_token();
        assert!(raw.starts_with(JOB_TOKEN_PREFIX) && raw.len() == JOB_TOKEN_PREFIX.len() + 64);
        assert_eq!(hash_job_token(&raw), hash_job_token(&raw));
        assert_ne!(hash_job_token(&raw), hash_job_token(&mint_job_token()));
        assert!(!hash_job_token(&raw).contains(&raw[JOB_TOKEN_PREFIX.len()..]));
    }

    #[test]
    fn a_token_outlives_the_timeout_by_an_hour_and_lasts_thirty_days_without_one() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 26)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(
            job_token_expiry(now, Some(600)),
            now + chrono::Duration::seconds(600 + 3600)
        );
        assert_eq!(
            job_token_expiry(now, None),
            now + chrono::Duration::days(30)
        );
    }
}
