/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]
#![allow(clippy::disallowed_methods, reason = "test harness server")]

//! A pull-through cache must serve build logs it does not hold itself (#547).
//!
//! Paths substituted from an upstream have no gradient build behind them, so
//! `nix log` against such a cache reported the log as missing. The cache now
//! asks its upstreams, in configured order, and serves the first hit.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use gradient_web::endpoints::caches::fetch_log_from_upstreams;
use tokio::net::TcpListener;

/// An upstream that answers `/log/{drv}` with `body`, or 404 when `body` is
/// `None`, counting the requests it received.
async fn upstream(body: Option<&'static str>) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/log/{drv}",
            get(
                |State((body, hits)): State<(Option<&'static str>, Arc<AtomicUsize>)>,
                 Path(_drv): Path<String>| async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    match body {
                        Some(b) => (StatusCode::OK, b),
                        None => (StatusCode::NOT_FOUND, ""),
                    }
                },
            ),
        )
        .with_state((body, Arc::clone(&hits)));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    (format!("http://{addr}"), hits)
}

const DRV: &str = "1mpqffikzpszxw6zzi8s63a3srqd6swx-python3.14-ctranslate2-4.8.1.drv";

/// The same constructor the server uses. `reqwest::Client::new()` panics where
/// no system CA bundle exists (the nix build sandbox); `build_client` folds in
/// `webpki_roots`, so it builds with or without native certs.
fn client() -> reqwest::Client {
    gradient_util::http::build_client().expect("http client builds")
}

#[tokio::test]
async fn serves_the_log_from_the_first_upstream_that_has_it() {
    let (without, without_hits) = upstream(None).await;
    let (with, with_hits) = upstream(Some("@nix { \"action\": \"setPhase\" }\nbuilding\n")).await;

    let log = fetch_log_from_upstreams(&client(), &[without, with], DRV).await;

    assert_eq!(
        log.as_deref(),
        Some("@nix { \"action\": \"setPhase\" }\nbuilding\n")
    );
    assert_eq!(
        without_hits.load(Ordering::SeqCst),
        1,
        "the first upstream must be asked before falling through"
    );
    assert_eq!(with_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reports_no_log_when_no_upstream_has_one() {
    let (first, _) = upstream(None).await;
    let (second, _) = upstream(None).await;

    assert_eq!(
        fetch_log_from_upstreams(&client(), &[first, second], DRV).await,
        None
    );
}

/// A trailing slash on a configured upstream must not produce `//log/...`.
#[tokio::test]
async fn normalizes_a_trailing_slash_on_the_upstream_url() {
    let (base, hits) = upstream(Some("log body")).await;

    let log = fetch_log_from_upstreams(&client(), &[format!("{base}/")], DRV).await;

    assert_eq!(log.as_deref(), Some("log body"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
