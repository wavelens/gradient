/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Some binary caches are negotiating HTTP/2 and then resetting streams mid-body. A proxied NAR
//! then ends after its `200` went out. The caller is learning of the reset through
//! `on_http2_failure` and is switching that cache to HTTP/1.1. The broken body is resuming there by
//! `Range`.

use std::error::Error;
use std::future::Future;
use std::sync::OnceLock;

use bytes::Bytes;
use futures::stream::{self, BoxStream, StreamExt as _};
use reqwest::header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};

fn http1_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        super::http::download_client_builder()
            .http1_only()
            .build()
            .expect("failed to build the HTTP/1.1 download client")
    })
}

fn client(http1_only: bool) -> &'static Client {
    if http1_only {
        http1_client()
    } else {
        super::http::download_client()
    }
}

pub fn is_http2_error(err: &(dyn Error + 'static)) -> bool {
    let mut cause = Some(err);
    while let Some(e) = cause {
        if e.is::<h2::Error>() {
            return true;
        }
        cause = e.source();
    }
    false
}

pub async fn get(
    url: &str,
    http1_only: bool,
    on_http2_failure: impl FnOnce(),
    configure: impl Fn(RequestBuilder) -> RequestBuilder,
) -> reqwest::Result<Response> {
    match configure(client(http1_only).get(url)).send().await {
        Err(e) if is_http2_error(&e) => {
            on_http2_failure();
            configure(http1_client().get(url)).send().await
        }
        sent => sent,
    }
}

pub fn resumable_body(
    response: Response,
    on_http2_failure: impl FnOnce() + Send + 'static,
) -> BoxStream<'static, reqwest::Result<Bytes>> {
    if response.content_length().is_none() {
        return response.bytes_stream().boxed();
    }
    let landed = response.url().clone();
    splice(response.bytes_stream().boxed(), move |offset| async move {
        on_http2_failure();
        reopen_at(landed, offset).await
    })
    .boxed()
}

async fn reopen_at(url: Url, offset: u64) -> Option<BoxStream<'static, reqwest::Result<Bytes>>> {
    let response = http1_client()
        .get(url)
        .header(RANGE, format!("bytes={offset}-"))
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await
        .ok()?;
    resumes_at(&response, offset).then(|| response.bytes_stream().boxed())
}

fn resumes_at(response: &Response, offset: u64) -> bool {
    response.status() == StatusCode::PARTIAL_CONTENT
        && response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|range| range.starts_with(&format!("bytes {offset}-")))
}

struct Splice<E, R> {
    body: BoxStream<'static, Result<Bytes, E>>,
    offset: u64,
    reopen: Option<R>,
}

fn splice<E, R, Fut>(
    body: BoxStream<'static, Result<Bytes, E>>,
    reopen: R,
) -> impl futures::Stream<Item = Result<Bytes, E>>
where
    E: Error + 'static,
    R: FnOnce(u64) -> Fut,
    Fut: Future<Output = Option<BoxStream<'static, Result<Bytes, E>>>>,
{
    let state = Splice {
        body,
        offset: 0,
        reopen: Some(reopen),
    };
    stream::unfold(state, |mut s| async move {
        loop {
            match s.body.next().await? {
                Ok(chunk) => {
                    s.offset += chunk.len() as u64;
                    return Some((Ok(chunk), s));
                }
                Err(e) => {
                    let resumed = match s.reopen.take() {
                        Some(reopen) if is_http2_error(&e) => reopen(s.offset).await,
                        _ => None,
                    };
                    match resumed {
                        Some(body) => s.body = body,
                        None => return Some((Err(e), s)),
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Debug)]
    struct Wrapped(Box<dyn Error + Send + Sync>);

    impl fmt::Display for Wrapped {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "wrapped: {}", self.0)
        }
    }

    impl Error for Wrapped {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self.0.as_ref())
        }
    }

    fn reset() -> Wrapped {
        Wrapped(Box::new(h2::Error::from(h2::Reason::PROTOCOL_ERROR)))
    }

    fn refused() -> Wrapped {
        Wrapped(Box::new(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )))
    }

    fn body(
        items: Vec<Result<&'static str, Wrapped>>,
    ) -> BoxStream<'static, Result<Bytes, Wrapped>> {
        stream::iter(
            items
                .into_iter()
                .map(|i| i.map(|s| Bytes::from_static(s.as_bytes()))),
        )
        .boxed()
    }

    async fn collect(s: impl futures::Stream<Item = Result<Bytes, Wrapped>>) -> (Vec<u8>, bool) {
        let items: Vec<_> = s.collect().await;
        let failed = items.iter().any(Result::is_err);
        let bytes = items.into_iter().flatten().flatten().collect();
        (bytes, failed)
    }

    #[test]
    fn an_h2_error_is_found_under_the_transport_errors_wrapping_it() {
        assert!(is_http2_error(&Wrapped(Box::new(reset()))));
        assert!(!is_http2_error(&refused()));
    }

    #[tokio::test]
    async fn a_reset_body_resumes_from_the_bytes_already_sent() {
        let spliced = splice(body(vec![Ok("ab"), Err(reset())]), |offset| async move {
            assert_eq!(offset, 2);
            Some(body(vec![Ok("cd")]))
        });

        assert_eq!(collect(spliced).await, (b"abcd".to_vec(), false));
    }

    #[tokio::test]
    async fn only_an_http2_failure_is_resumed() {
        let spliced = splice(body(vec![Ok("ab"), Err(refused())]), |_| async {
            panic!("a non-HTTP/2 failure must reach the client as itself")
        });

        assert_eq!(collect(spliced).await, (b"ab".to_vec(), true));
    }

    #[tokio::test]
    async fn a_body_is_resumed_at_most_once() {
        let spliced = splice(body(vec![Ok("ab"), Err(reset())]), |_| async {
            Some(body(vec![Ok("c"), Err(reset())]))
        });

        assert_eq!(collect(spliced).await, (b"abc".to_vec(), true));
    }

    #[tokio::test]
    async fn a_resume_continues_only_from_a_matching_range() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ranged.nar"))
            .and(header("range", "bytes=2-"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-range", "bytes 2-3/4")
                    .set_body_bytes(b"cd".to_vec()),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/unranged.nar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abcd".to_vec()))
            .mount(&server)
            .await;
        let url = |p: &str| Url::parse(&format!("{}{p}", server.uri())).unwrap();

        let resumed = reopen_at(url("/ranged.nar"), 2).await.expect("206 resumes");
        let rest: Vec<Bytes> = resumed.map(|c| c.unwrap()).collect().await;
        assert_eq!(rest.concat(), b"cd");
        assert!(
            reopen_at(url("/unranged.nar"), 2).await.is_none(),
            "a server ignoring Range would splice the whole body after its start"
        );
    }
}
