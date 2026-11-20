/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_entity::StoreHash;
use gradient_types::{ServerArgs, frontend_page_urls};
use std::sync::Arc;

pub(crate) fn request_accepts_html_explicitly(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|media_range| {
            media_range
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("text/html")
        })
}

pub(crate) async fn redirect_browser_to_cache_frontend_page(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<String>, PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let target = path
        .ok()
        .map(|Path(cache)| frontend_page_urls::cache(&state.config.server.frontend_url, &cache));
    redirect_browser_html_request_to_binary_cache_url(&state.config.server, target, request, next)
        .await
}

pub(crate) async fn redirect_browser_to_cached_store_path_page(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<(String, String)>, PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let target = path.ok().and_then(|Path((cache, file))| {
        let hash = file.strip_suffix(".narinfo")?;
        frontend_store_path_page_url_for_cache_and_hash(&state.config.server, &cache, hash)
    });
    redirect_browser_html_request_to_binary_cache_url(&state.config.server, target, request, next)
        .await
}

pub(crate) async fn redirect_browser_from_nar_listing_to_store_path_page(
    State(state): State<Arc<ServerState>>,
    path: Result<Path<(String, String)>, PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let target = path.ok().and_then(|Path((cache, hash))| {
        frontend_store_path_page_url_for_cache_and_hash(&state.config.server, &cache, &hash)
    });
    redirect_browser_html_request_to_binary_cache_url(&state.config.server, target, request, next)
        .await
}

fn frontend_store_path_page_url_for_cache_and_hash(
    server: &ServerArgs,
    cache: &str,
    hash: &str,
) -> Option<String> {
    let hash = StoreHash::parse(hash).ok()?;
    Some(frontend_page_urls::cached_store_path_page_url(
        &server.frontend_url,
        cache,
        &hash.to_string(),
    ))
}

async fn redirect_browser_html_request_to_binary_cache_url(
    server: &ServerArgs,
    target: Option<String>,
    request: Request,
    next: Next,
) -> Response {
    if !server.frontend_redirect_browser_requests_to_frontend_pages {
        return next.run(request).await;
    }

    let location = target
        .filter(|_| request_accepts_html_explicitly(request.headers()))
        .and_then(|target| HeaderValue::try_from(target).ok());
    let mut response = match location {
        Some(location) => (
            StatusCode::FOUND,
            [
                (header::LOCATION, location),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
        )
            .into_response(),
        None => next.run(request).await,
    };
    add_accept_to_vary_header(response.headers_mut());
    response
}

fn add_accept_to_vary_header(headers: &mut HeaderMap) {
    let mut names: Vec<String> = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    if names
        .iter()
        .any(|name| name == "*" || name.eq_ignore_ascii_case("accept"))
    {
        return;
    }
    names.push("Accept".to_owned());
    if let Ok(value) = HeaderValue::from_str(&names.join(", ")) {
        headers.insert(header::VARY, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_map_with_accept_value(accept: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
        headers
    }

    #[test]
    fn firefox_chromium_safari_lynx_w3m_accept_values_count_as_html_page_requests() {
        for accept in [
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "text/html, text/plain, text/sgml, text/css, application/xhtml+xml, */*;q=0.01",
            "text/html, text/*;q=0.5, image/*",
        ] {
            assert!(
                request_accepts_html_explicitly(&header_map_with_accept_value(accept)),
                "{accept}"
            );
        }
    }

    #[test]
    fn accept_values_of_nix_curl_wget_and_fetch_call_do_not_count_as_html_page_requests() {
        for (client, accept) in [
            ("nix", "*/*"),
            ("curl", "*/*"),
            ("wget", "*/*"),
            ("fetch", "*/*"),
            ("fetch json", "application/json, text/plain, */*"),
        ] {
            assert!(
                !request_accepts_html_explicitly(&header_map_with_accept_value(accept)),
                "{client}: {accept}"
            );
        }
        assert!(!request_accepts_html_explicitly(&HeaderMap::new()));
    }

    #[test]
    fn test_adding_accept_to_vary_keeps_existing_header_names_without_duplicating_accept() {
        let mut headers = HeaderMap::new();
        headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        add_accept_to_vary_header(&mut headers);
        assert_eq!(headers[header::VARY], "Accept-Encoding, Accept");
        add_accept_to_vary_header(&mut headers);
        assert_eq!(headers[header::VARY], "Accept-Encoding, Accept");
    }
}
