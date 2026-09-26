/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every PUT this worker sends to object storage: bounded by one worker-wide
//! permit pool across all jobs, and retried with jittered exponential backoff
//! when the store throttles (503 / 429), errors (5xx) or drops the connection.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::header::{CONTENT_TYPE, ETAG, HeaderMap, RETRY_AFTER};
use tokio::sync::Semaphore;

const DEFAULT_CONCURRENT_PUTS: usize = 8;

static PERMITS: OnceLock<Semaphore> = OnceLock::new();

pub(crate) fn limit_concurrent_puts(limit: usize) {
    if PERMITS.set(Semaphore::new(limit)).is_err() {
        tracing::warn!(limit, "object PUT limit already set; keeping the first");
    }
}

fn permits() -> &'static Semaphore {
    PERMITS.get_or_init(|| Semaphore::new(DEFAULT_CONCURRENT_PUTS))
}

pub(crate) struct Backoff {
    pub attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Backoff {
    fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(requested) = retry_after {
            return requested.min(self.max);
        }
        let ceiling = self
            .base
            .saturating_mul(1u32 << (attempt - 1).min(16))
            .min(self.max);
        ceiling.mul_f64(rand::random_range(0.5..=1.0))
    }
}

const OBJECT_STORE_BACKOFF: Backoff = Backoff {
    attempts: 6,
    base: Duration::from_secs(1),
    max: Duration::from_secs(30),
};

/// PUT `body` to a presigned `url`; returns the object's ETag when the store sent one.
pub(crate) async fn put_object(
    url: &str,
    body: Bytes,
    content_type: Option<&str>,
) -> Result<Option<String>> {
    put_with(permits(), &OBJECT_STORE_BACKOFF, url, body, content_type).await
}

async fn put_with(
    permits: &Semaphore,
    backoff: &Backoff,
    url: &str,
    body: Bytes,
    content_type: Option<&str>,
) -> Result<Option<String>> {
    let mut attempt = 1;
    loop {
        let outcome = {
            let _permit = permits
                .acquire()
                .await
                .context("object PUT limiter closed")?;
            try_put(url, body.clone(), content_type).await
        };
        match outcome {
            Ok(etag) => return Ok(etag),
            Err(PutError::Retryable { error, retry_after }) if attempt < backoff.attempts => {
                let delay = backoff.delay(attempt, retry_after);
                tracing::warn!(attempt, delay_ms = delay.as_millis() as u64, error = %error, "object PUT failed; retrying");
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(PutError::Retryable { error, .. } | PutError::Fatal(error)) => {
                return Err(error.context(format!("object PUT gave up after {attempt} attempt(s)")));
            }
        }
    }
}

enum PutError {
    Retryable {
        error: anyhow::Error,
        retry_after: Option<Duration>,
    },
    Fatal(anyhow::Error),
}

async fn try_put(
    url: &str,
    body: Bytes,
    content_type: Option<&str>,
) -> std::result::Result<Option<String>, PutError> {
    let mut request = gradient_worker_client::http::client().put(url).body(body);
    if let Some(content_type) = content_type {
        request = request.header(CONTENT_TYPE, content_type);
    }
    let resp = request.send().await.map_err(|e| PutError::Retryable {
        error: anyhow::Error::new(e).context("object PUT failed to send"),
        retry_after: None,
    })?;

    let status = resp.status();
    if status.is_success() {
        return Ok(resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned));
    }

    let retry_after = retry_after(resp.headers());
    let error = anyhow::anyhow!(
        "object PUT returned {status}: {}",
        resp.text().await.unwrap_or_default()
    );
    Err(
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            PutError::Retryable { error, retry_after }
        } else {
            PutError::Fatal(error)
        },
    )
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const IMMEDIATE: Backoff = Backoff {
        attempts: 3,
        base: Duration::ZERO,
        max: Duration::ZERO,
    };

    async fn requests(server: &MockServer) -> usize {
        server.received_requests().await.unwrap().len()
    }

    #[tokio::test]
    async fn a_throttled_put_is_retried_until_it_lands() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "1"))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"obj\""))
            .mount(&server)
            .await;

        let etag = put_with(
            &Semaphore::new(1),
            &IMMEDIATE,
            &server.uri(),
            Bytes::from_static(b"nar"),
            None,
        )
        .await
        .unwrap();

        assert_eq!(etag.as_deref(), Some("\"obj\""));
        assert_eq!(requests(&server).await, 3);
    }

    #[tokio::test]
    async fn a_put_that_stays_throttled_fails_after_its_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&server)
            .await;

        let err = put_with(
            &Semaphore::new(1),
            &IMMEDIATE,
            &server.uri(),
            Bytes::from_static(b"nar"),
            None,
        )
        .await
        .unwrap_err();

        assert!(format!("{err:#}").contains("503"), "{err:#}");
    }

    #[tokio::test]
    async fn a_rejected_put_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let err = put_with(
            &Semaphore::new(1),
            &IMMEDIATE,
            &server.uri(),
            Bytes::from_static(b"nar"),
            None,
        )
        .await
        .unwrap_err();

        assert!(format!("{err:#}").contains("403"), "{err:#}");
    }

    #[tokio::test]
    async fn a_put_waits_for_a_free_permit() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let permits = Semaphore::new(1);
        let held = permits.acquire().await.unwrap();
        let url = server.uri();

        let (put, sent_while_held) = tokio::join!(
            put_with(&permits, &IMMEDIATE, &url, Bytes::from_static(b"nar"), None),
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let sent = requests(&server).await;
                drop(held);
                sent
            }
        );

        put.unwrap();
        assert_eq!(sent_while_held, 0);
        assert_eq!(requests(&server).await, 1);
    }

    #[test]
    fn retry_after_is_honoured_up_to_the_cap() {
        let backoff = Backoff {
            attempts: 6,
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        };
        assert_eq!(
            backoff.delay(1, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            backoff.delay(1, Some(Duration::from_secs(600))),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn the_backoff_doubles_per_attempt_within_its_jitter() {
        let backoff = Backoff {
            attempts: 6,
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        };
        let third = backoff.delay(3, None);
        assert!(
            (Duration::from_secs(2)..=Duration::from_secs(4)).contains(&third),
            "{third:?}"
        );
        let late = backoff.delay(12, None);
        assert!(
            (Duration::from_secs(15)..=Duration::from_secs(30)).contains(&late),
            "{late:?}"
        );
    }
}
