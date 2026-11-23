/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use gradient_wire::constants::WRITE_STALL_TIMEOUT;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderMap, RETRY_AFTER};
use tokio::sync::watch;
use tokio::time::Instant;

pub struct Backoff {
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

const UPLOAD_BODY_SLICE_SIZE_BYTES: usize = 256 * 1024;

const OBJECT_STORE_BACKOFF: Backoff = Backoff {
    attempts: 6,
    base: Duration::from_secs(1),
    max: Duration::from_secs(30),
};

pub async fn put_object(
    url: &str,
    body: Bytes,
    content_type: Option<&str>,
) -> Result<Option<String>> {
    put_with(
        &OBJECT_STORE_BACKOFF,
        WRITE_STALL_TIMEOUT,
        url,
        body,
        content_type,
    )
    .await
}

async fn put_with(
    backoff: &Backoff,
    write_stall_timeout: Duration,
    url: &str,
    body: Bytes,
    content_type: Option<&str>,
) -> Result<Option<String>> {
    let mut attempt = 1;
    loop {
        match try_put(url, body.clone(), content_type, write_stall_timeout).await {
            Ok(etag) => return Ok(etag),
            Err(PutError::Retryable { error, retry_after }) if attempt < backoff.attempts => {
                let delay = backoff.delay(attempt, retry_after);
                tracing::warn!(attempt, delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX), error = %error, "object PUT failed; retrying");
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(PutError::Retryable { error, .. }) => {
                return Err(error
                    .context(crate::connection::Unresponsive)
                    .context(format!("object PUT gave up after {attempt} attempt(s)")));
            }
            Err(PutError::Fatal(error)) => {
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
    write_stall_timeout: Duration,
) -> std::result::Result<Option<String>, PutError> {
    let total = body.len();
    let (body_bytes_uploaded_watch_sender, body_bytes_uploaded_watch) = watch::channel(0);
    let slices = (0..total)
        .step_by(UPLOAD_BODY_SLICE_SIZE_BYTES)
        .map(move |start| {
            let end = (start + UPLOAD_BODY_SLICE_SIZE_BYTES).min(total);
            body_bytes_uploaded_watch_sender.send_replace(end);
            Ok::<_, std::convert::Infallible>(body.slice(start..end))
        });
    let mut request = crate::http::presigned_object_upload_client()
        .put(url)
        .header(CONTENT_LENGTH, total)
        .body(reqwest::Body::wrap_stream(futures::stream::iter(slices)));
    if let Some(content_type) = content_type {
        request = request.header(CONTENT_TYPE, content_type);
    }
    let resp = tokio::select! {
        response = request.send() => response.map_err(|e| PutError::Retryable {
            error: anyhow::Error::new(e).context("send the object PUT"),
            retry_after: None,
        })?,
        () = wait_for_no_progress_for_stall_window(write_stall_timeout, body_bytes_uploaded_watch.clone()) => {
            return Err(PutError::Retryable {
                error: anyhow::anyhow!(
                    "No upload progress for {write_stall_timeout:?} after {sent} of {total} bytes.",
                    sent = *body_bytes_uploaded_watch.borrow()
                ),
                retry_after: None,
            });
        }
    };

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
        tokio::time::timeout(write_stall_timeout, resp.text())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    );
    Err(
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            PutError::Retryable { error, retry_after }
        } else {
            PutError::Fatal(error)
        },
    )
}

async fn wait_for_no_progress_for_stall_window(
    write_stall_timeout: Duration,
    mut body_bytes_uploaded_watch: watch::Receiver<usize>,
) {
    let mut deadline = Instant::now() + write_stall_timeout;
    while let Ok(Ok(())) =
        tokio::time::timeout_at(deadline, body_bytes_uploaded_watch.changed()).await
    {
        deadline = Instant::now() + write_stall_timeout;
    }
    tokio::time::sleep_until(deadline).await;
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

    const SHORT_STALL_WINDOW_DURATION: Duration = Duration::from_secs(1);

    const SINGLE_ATTEMPT_BACKOFF: Backoff = Backoff {
        attempts: 1,
        base: Duration::ZERO,
        max: Duration::ZERO,
    };

    async fn requests(server: &MockServer) -> usize {
        server.received_requests().await.unwrap().len()
    }

    fn build_upload_body_of_length_with_repeating_byte_pattern(len: usize) -> Bytes {
        Bytes::from((0..=250u8).collect::<Vec<u8>>().repeat(len.div_ceil(251))).slice(..len)
    }

    fn start_http_peer_that_handles_one_request(
        handle: impl FnOnce(std::net::TcpStream, String, Vec<u8>) + Send + 'static,
    ) -> String {
        use std::io::Read as _;

        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
            .expect("socket");
        socket
            .set_recv_buffer_size(256 << 10)
            .expect("receive buffer");
        socket
            .bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into())
            .expect("bind");
        socket.listen(1).expect("listen");
        let listener = std::net::TcpListener::from(socket);
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            let head_end = loop {
                let n = stream.read(&mut buf).expect("read the request head");
                assert!(n > 0, "connection closed before the request head ended");
                received.extend_from_slice(&buf[..n]);
                if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let body_start = received.split_off(head_end);
            let head = String::from_utf8_lossy(&received).to_ascii_lowercase();
            handle(stream, head, body_start);
        });

        format!("http://{addr}/")
    }

    fn read_request_body_from_tcp_stream_until_total_byte_count(
        stream: &mut std::net::TcpStream,
        mut already: usize,
        len: usize,
    ) {
        use std::io::Read as _;

        let mut buf = vec![0u8; 1 << 20];
        while already < len {
            let n = stream.read(&mut buf).expect("read the body");
            assert!(n > 0, "connection closed before the body ended");
            already += n;
        }
    }

    #[tokio::test]
    async fn test_object_put_to_slow_steady_server_reading_across_multiple_stall_windows_succeeds_with_content_length_not_chunked_and_exact_bytes()
     {
        use std::io::{Read as _, Write as _};

        let body = build_upload_body_of_length_with_repeating_byte_pattern(144 << 20);
        let len = body.len();
        let expected = body.clone();
        let (report, received) = std::sync::mpsc::channel();
        let url = start_http_peer_that_handles_one_request(move |mut stream, head, start| {
            let started = std::time::Instant::now();
            let mut identical = start[..] == expected[..start.len()];
            let mut total = start.len();
            let mut buf = vec![0u8; 256 << 10];
            while total < len {
                let want = buf.len().min(len - total);
                let n = stream.read(&mut buf[..want]).expect("read the body");
                assert!(n > 0, "connection closed before the body ended");
                identical &= buf[..n] == expected[total..total + n];
                total += n;
                let due = Duration::from_secs_f64(total as f64 / f64::from(48u32 << 20));
                std::thread::sleep(due.saturating_sub(started.elapsed()));
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nETag: \"steady\"\r\nContent-Length: 0\r\n\r\n");
            let _ = report.send((head, total, identical));
        });

        let started = std::time::Instant::now();
        let etag = put_with(
            &SINGLE_ATTEMPT_BACKOFF,
            SHORT_STALL_WINDOW_DURATION,
            &url,
            body.clone(),
            None,
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();

        let (head, total, identical) = received.recv().unwrap();
        assert_eq!(etag.as_deref(), Some("\"steady\""));
        assert!(
            head.contains(&format!("content-length: {len}\r\n")),
            "{head}"
        );
        assert!(!head.contains("transfer-encoding"), "{head}");
        assert_eq!(total, len);
        assert!(identical, "the server received other bytes than were sent");
        assert!(
            elapsed > SHORT_STALL_WINDOW_DURATION * 2,
            "the upload must span several stall windows: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn object_put_fails_as_retryable_stall_naming_progress_when_server_stops_reading_after_request_head()
     {
        let upload_body_size_bytes: usize = 32 << 20;
        let body = build_upload_body_of_length_with_repeating_byte_pattern(upload_body_size_bytes);
        let url = start_http_peer_that_handles_one_request(|stream, _, _| {
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        });

        let started = std::time::Instant::now();
        let err = put_with(
            &SINGLE_ATTEMPT_BACKOFF,
            SHORT_STALL_WINDOW_DURATION,
            &url,
            body,
            None,
        )
        .await
        .unwrap_err();

        assert!(
            started.elapsed() < SHORT_STALL_WINDOW_DURATION * 10,
            "{:?}",
            started.elapsed()
        );
        assert!(
            err.downcast_ref::<crate::connection::Unresponsive>()
                .is_some(),
            "{err:#}"
        );
        let message = format!("{err:#}");
        assert!(
            message.contains(&format!("of {upload_body_size_bytes} bytes")),
            "{message}"
        );
        assert!(
            message.contains(&format!("for {SHORT_STALL_WINDOW_DURATION:?} after")),
            "{message}"
        );
        let body_bytes_uploaded_before_stall: usize = err
            .root_cause()
            .to_string()
            .split_once(" after ")
            .and_then(|(_, rest)| rest.split_once(" of "))
            .and_then(|(sent, _)| sent.parse().ok())
            .unwrap_or_else(|| panic!("{message}"));
        assert!(
            body_bytes_uploaded_before_stall < upload_body_size_bytes,
            "{message}"
        );
    }

    #[tokio::test]
    async fn object_put_fails_as_retryable_stall_when_server_reads_whole_body_but_sends_no_response()
     {
        let len = 1 << 20;
        let body = build_upload_body_of_length_with_repeating_byte_pattern(len);
        let url = start_http_peer_that_handles_one_request(move |mut stream, _, start| {
            read_request_body_from_tcp_stream_until_total_byte_count(&mut stream, start.len(), len);
            std::thread::sleep(Duration::from_secs(30));
        });

        let started = std::time::Instant::now();
        let err = put_with(
            &SINGLE_ATTEMPT_BACKOFF,
            SHORT_STALL_WINDOW_DURATION,
            &url,
            body,
            None,
        )
        .await
        .unwrap_err();

        assert!(
            started.elapsed() < SHORT_STALL_WINDOW_DURATION * 10,
            "{:?}",
            started.elapsed()
        );
        assert!(
            err.downcast_ref::<crate::connection::Unresponsive>()
                .is_some(),
            "{err:#}"
        );
        assert!(
            format!("{err:#}").contains(&format!("{len} of {len} bytes")),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn put_empty_and_small_body_succeeds_and_returns_server_etag() {
        for body in [Bytes::new(), Bytes::from_static(b"nar")] {
            let server = MockServer::start().await;
            Mock::given(method("PUT"))
                .and(wiremock::matchers::header(
                    "content-length",
                    body.len().to_string(),
                ))
                .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"obj\""))
                .expect(1)
                .mount(&server)
                .await;

            let etag = put_with(
                &IMMEDIATE,
                WRITE_STALL_TIMEOUT,
                &server.uri(),
                body.clone(),
                None,
            )
            .await
            .unwrap();

            assert_eq!(etag.as_deref(), Some("\"obj\""));
            let received = server.received_requests().await.unwrap();
            assert_eq!(received[0].body, body.as_ref());
        }
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
            &IMMEDIATE,
            WRITE_STALL_TIMEOUT,
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
            &IMMEDIATE,
            WRITE_STALL_TIMEOUT,
            &server.uri(),
            Bytes::from_static(b"nar"),
            None,
        )
        .await
        .unwrap_err();

        assert!(format!("{err:#}").contains("503"), "{err:#}");
        assert!(
            err.downcast_ref::<crate::connection::Unresponsive>()
                .is_some(),
            "a store that keeps failing is an outage, not a verdict on the job"
        );
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
            &IMMEDIATE,
            WRITE_STALL_TIMEOUT,
            &server.uri(),
            Bytes::from_static(b"nar"),
            None,
        )
        .await
        .unwrap_err();

        assert!(format!("{err:#}").contains("403"), "{err:#}");
        assert!(
            err.downcast_ref::<crate::connection::Unresponsive>()
                .is_none()
        );
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
