/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LONGEST_RETRY_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct OutboundHttpNonSuccessStatusError {
    pub status: StatusCode,
    pub retry_after_duration: Option<Duration>,
    pub message: String,
    rate_limited: bool,
}

impl OutboundHttpNonSuccessStatusError {
    pub fn create_error_from_response_status_and_headers(
        status: StatusCode,
        headers: &HeaderMap,
        message: String,
    ) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        let reset_at = header("x-ratelimit-reset").or_else(|| header("ratelimit-reset"));
        let wait_until_reset = reset_at.map(|reset| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            Duration::from_secs(reset.saturating_sub(now))
        });
        let rate_limited = is_rate_limited(status, headers);
        let retry_after_duration = header("retry-after")
            .map(Duration::from_secs)
            .or(wait_until_reset.filter(|_| rate_limited))
            .map(|wait| wait.min(LONGEST_RETRY_AFTER));

        Self {
            status,
            retry_after_duration,
            message,
            rate_limited,
        }
    }

    pub fn is_not_retryable(&self) -> bool {
        self.status.is_client_error()
            && !self.rate_limited
            && !matches!(
                self.status,
                StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
            )
    }
}

fn is_rate_limited(status: StatusCode, headers: &HeaderMap) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && headers
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim() == "0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(status: u16, headers: &[(&'static str, String)]) -> OutboundHttpNonSuccessStatusError {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(*name, value.parse().unwrap());
        }
        OutboundHttpNonSuccessStatusError::create_error_from_response_status_and_headers(
            StatusCode::from_u16(status).unwrap(),
            &map,
            String::new(),
        )
    }

    #[test]
    fn client_errors_are_final_and_server_errors_are_retried() {
        assert!(error(422, &[]).is_not_retryable());
        assert!(error(404, &[]).is_not_retryable());
        assert!(!error(502, &[]).is_not_retryable());
        assert!(!error(429, &[]).is_not_retryable());
        assert!(!error(408, &[]).is_not_retryable());
    }

    #[test]
    fn retry_after_is_honoured_and_capped() {
        let wait = error(429, &[("retry-after", "120".into())]).retry_after_duration;
        assert_eq!(wait, Some(Duration::from_secs(120)));
        let wait = error(503, &[("retry-after", "86400".into())]).retry_after_duration;
        assert_eq!(wait, Some(LONGEST_RETRY_AFTER));
        assert_eq!(error(502, &[]).retry_after_duration, None);
    }

    #[test]
    fn an_exhausted_rate_limit_waits_for_its_reset() {
        let reset = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        let limited = error(
            403,
            &[
                ("x-ratelimit-remaining", "0".into()),
                ("x-ratelimit-reset", reset.to_string()),
            ],
        );
        assert!(!limited.is_not_retryable());
        let wait = limited.retry_after_duration.unwrap();
        assert!(wait > Duration::from_secs(590) && wait <= Duration::from_secs(600));

        assert!(error(403, &[("x-ratelimit-reset", reset.to_string())]).is_not_retryable());
    }
}
