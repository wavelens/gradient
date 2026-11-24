/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::error::Error as _;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::TryStreamExt as _;
use gradient_util::http::{
    body_from_byte_chunks_with_handed_byte_count_watch, wait_for_no_progress_for_stall_window,
};
use http_body::{Frame, SizeHint};
use http_body_util::BodyExt as _;
use object_store::client::{
    HttpError, HttpErrorKind, HttpRequest, HttpResponse, HttpResponseBody, HttpService,
};
use reqwest::header::CONTENT_LENGTH;

#[derive(Debug)]
pub(crate) struct NoProgressTimeoutHttpService {
    pub(crate) client: reqwest::Client,
    pub(crate) no_progress_timeout: Duration,
}

#[async_trait::async_trait]
impl HttpService for NoProgressTimeoutHttpService {
    async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let (parts, body) = request.into_parts();
        let url = reqwest::Url::parse(&parts.uri.to_string())
            .map_err(|e| HttpError::new(HttpErrorKind::Unknown, e))?;
        let total = body.content_length();
        let mut request = reqwest::Request::new(parts.method, url);
        *request.headers_mut() = parts.headers;
        let handed_byte_count = if total == 0 {
            *request.body_mut() = Some(reqwest::Body::from(Bytes::new()));
            tokio::sync::watch::channel(0).1
        } else {
            let chunks = body.into_data_stream().try_collect::<Vec<Bytes>>().await?;
            let (upload_body, handed_byte_count) =
                body_from_byte_chunks_with_handed_byte_count_watch(chunks);
            request
                .headers_mut()
                .entry(CONTENT_LENGTH)
                .or_insert_with(|| total.into());
            *request.body_mut() = Some(upload_body);
            handed_byte_count
        };

        let window = self.no_progress_timeout;
        let response = tokio::select! {
            response = self.client.execute(request) => {
                response.map_err(convert_reqwest_error_to_object_store_http_error)?
            }
            () = wait_for_no_progress_for_stall_window(window, handed_byte_count.clone()) => {
                let sent = *handed_byte_count.borrow();
                return Err(HttpError::new(
                    HttpErrorKind::Timeout,
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("S3 upload sent {sent} of {total} bytes, then made no progress for {window:?}."),
                    ),
                ));
            }
        };

        let (parts, body) = http::Response::<reqwest::Body>::from(response).into_parts();
        Ok(HttpResponse::from_parts(
            parts,
            HttpResponseBody::new(S3ResponseBodyWithChunkIdleTimeout {
                body,
                no_progress_timeout: window,
                deadline: None,
            }),
        ))
    }
}

struct S3ResponseBodyWithChunkIdleTimeout {
    body: reqwest::Body,
    no_progress_timeout: Duration,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl http_body::Body for S3ResponseBodyWithChunkIdleTimeout {
    type Data = Bytes;
    type Error = HttpError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, HttpError>>> {
        let this = &mut *self;
        if let Poll::Ready(frame) = Pin::new(&mut this.body).poll_frame(cx) {
            this.deadline = None;
            return Poll::Ready(
                frame.map(|frame| frame.map_err(convert_reqwest_error_to_object_store_http_error)),
            );
        }

        let window = this.no_progress_timeout;
        let deadline = this
            .deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(window)));
        match deadline.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Some(Err(HttpError::new(
                HttpErrorKind::Timeout,
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("No response data from S3 for {window:?}."),
                ),
            )))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

fn convert_reqwest_error_to_object_store_http_error(error: reqwest::Error) -> HttpError {
    let mut kind = if error.is_timeout() {
        HttpErrorKind::Timeout
    } else if error.is_connect() {
        HttpErrorKind::Connect
    } else if error.is_decode() {
        HttpErrorKind::Decode
    } else {
        HttpErrorKind::Unknown
    };

    let mut source = error.source();
    while kind == HttpErrorKind::Unknown {
        let Some(cause) = source else {
            break;
        };
        if let Some(hyper_error) = cause.downcast_ref::<hyper::Error>() {
            if hyper_error.is_closed()
                || hyper_error.is_incomplete_message()
                || hyper_error.is_body_write_aborted()
            {
                kind = HttpErrorKind::Request;
            } else if hyper_error.is_timeout() {
                kind = HttpErrorKind::Timeout;
            }
        }
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            match io_error.kind() {
                std::io::ErrorKind::TimedOut => kind = HttpErrorKind::Timeout,
                std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof => kind = HttpErrorKind::Interrupted,
                _ => {}
            }
        }
        source = cause.source();
    }

    HttpError::new(kind, error.without_url())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_util::http::start_http_peer_that_handles_one_request;
    use object_store::client::HttpRequestBody;
    use std::io::{Read as _, Write as _};

    const SHORT_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(1);

    fn service() -> NoProgressTimeoutHttpService {
        NoProgressTimeoutHttpService {
            client: gradient_util::http::untimed_client_builder()
                .build()
                .expect("client"),
            no_progress_timeout: SHORT_NO_PROGRESS_TIMEOUT,
        }
    }

    fn request(method: &str, url: &str, body: HttpRequestBody) -> HttpRequest {
        http::Request::builder()
            .method(method)
            .uri(url)
            .body(body)
            .expect("request")
    }

    fn repeating_byte_pattern(len: usize) -> Bytes {
        Bytes::from((0..=250u8).collect::<Vec<u8>>().repeat(len.div_ceil(251))).slice(..len)
    }

    #[tokio::test]
    async fn slow_upload_read_across_more_than_two_no_progress_windows_completes_with_exact_bytes_and_content_length()
     {
        let body = repeating_byte_pattern(24 << 20);
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
                let due = Duration::from_secs_f64(total as f64 / f64::from(8u32 << 20));
                std::thread::sleep(due.saturating_sub(started.elapsed()));
            }
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            let _ = report.send((head, total, identical));
        });
        let payload: object_store::PutPayload = (0..len)
            .step_by(5 << 20)
            .map(|start| body.slice(start..(start + (5 << 20)).min(len)))
            .collect();

        let started = std::time::Instant::now();
        let response = service()
            .call(request("PUT", &url, payload.into()))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        let (head, total, identical) = received.recv().unwrap();
        assert!(response.status().is_success());
        assert!(
            head.contains(&format!("content-length: {len}\r\n")),
            "{head}"
        );
        assert!(!head.contains("transfer-encoding"), "{head}");
        assert_eq!(total, len);
        assert!(identical, "the server received other bytes than were sent");
        assert!(
            elapsed > SHORT_NO_PROGRESS_TIMEOUT * 2,
            "the upload must span several windows: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn request_fails_with_timeout_http_error_when_server_stops_reading_upload_body_after_no_progress_window()
     {
        let url = start_http_peer_that_handles_one_request(|stream, _, _| {
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        });

        let started = std::time::Instant::now();
        let err = service()
            .call(request(
                "PUT",
                &url,
                repeating_byte_pattern(32 << 20).into(),
            ))
            .await
            .unwrap_err();
        let elapsed = started.elapsed();

        assert_eq!(err.kind(), HttpErrorKind::Timeout, "{err}");
        assert!(
            (SHORT_NO_PROGRESS_TIMEOUT..SHORT_NO_PROGRESS_TIMEOUT * 5).contains(&elapsed),
            "{elapsed:?}"
        );
    }

    #[tokio::test]
    async fn reading_response_body_that_stops_delivering_data_fails_with_timeout_error() {
        let url = start_http_peer_that_handles_one_request(|mut stream, _, _| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhalf");
            std::thread::sleep(Duration::from_secs(30));
        });

        let response = service()
            .call(request("GET", &url, HttpRequestBody::empty()))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let err = response.into_body().bytes().await.unwrap_err();

        assert_eq!(err.kind(), HttpErrorKind::Timeout, "{err}");
        assert!(
            started.elapsed() < SHORT_NO_PROGRESS_TIMEOUT * 5,
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn test_get_request_sends_no_body_and_no_chunked_transfer_encoding() {
        let (report, received) = std::sync::mpsc::channel();
        let url = start_http_peer_that_handles_one_request(move |mut stream, head, start| {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            let _ = report.send((head, start));
        });

        let response = service()
            .call(request("GET", &url, HttpRequestBody::empty()))
            .await
            .unwrap();
        let body = response.into_body().bytes().await.unwrap();

        let (head, start) = received.recv().unwrap();
        assert_eq!(body.as_ref(), b"ok");
        assert!(!head.contains("transfer-encoding"), "{head}");
        assert!(!head.contains("content-length"), "{head}");
        assert!(start.is_empty());
    }

    #[tokio::test]
    async fn test_s3_put_sends_same_content_length_as_object_store_without_chunked_encoding() {
        use object_store::ObjectStoreExt as _;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"obj\""))
            .mount(&server)
            .await;
        let store = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name("bucket")
            .with_region("us-east-1")
            .with_endpoint(server.uri())
            .with_allow_http(true)
            .with_access_key_id("key")
            .with_secret_access_key("secret")
            .with_http_connector(crate::nar::SharedHttpConnector {
                no_progress_timeout: SHORT_NO_PROGRESS_TIMEOUT,
                allow_http: true,
            })
            .build()
            .unwrap();
        let body = repeating_byte_pattern(3 << 20);
        let payload: object_store::PutPayload = [body.slice(..1 << 20), body.slice(1 << 20..)]
            .into_iter()
            .collect();

        store
            .put(&object_store::path::Path::from("nar"), payload)
            .await
            .unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let headers = &received[0].headers;
        assert_eq!(
            headers.get("content-length").map(|v| v.as_bytes()),
            Some(body.len().to_string().as_bytes())
        );
        assert!(headers.get("transfer-encoding").is_none(), "{headers:?}");
        assert_eq!(received[0].body, body.as_ref());
    }
}
