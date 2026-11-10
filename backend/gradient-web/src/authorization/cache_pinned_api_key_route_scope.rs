/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::AccessRefusal;
use axum::extract::MatchedPath;
use axum::http::request::Parts;

use super::api_key::ApiKeyContext;

const CACHE_KEY_ROUTES: &[&str] = &["/caches", "/user/keys/permissions"];
const CACHE_KEY_PREFIX: &str = "/caches/{cache}";

pub(crate) fn api_key_cache_scope_allows_route(matched_path: &str) -> Result<(), AccessRefusal> {
    let route = matched_path.strip_prefix("/api/v1").unwrap_or(matched_path);
    let under_cache = route
        .strip_prefix(CACHE_KEY_PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
    if under_cache || CACHE_KEY_ROUTES.contains(&route) {
        Ok(())
    } else {
        Err(AccessRefusal::CachePinnedKeyOnEndpoint)
    }
}

pub(crate) fn cache_pinned_key_scope_of_request(
    parts: &Parts,
    api_key: Option<&ApiKeyContext>,
) -> Result<(), AccessRefusal> {
    if api_key.is_none_or(|key| key.cache_pin.is_none()) {
        return Ok(());
    }
    let matched_path = parts
        .extensions
        .get::<MatchedPath>()
        .ok_or(AccessRefusal::CachePinnedKeyOnEndpoint)?;
    api_key_cache_scope_allows_route(matched_path.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cache_key_reaches_its_cache_routes() {
        for path in [
            "/api/v1/caches",
            "/api/v1/caches/{cache}",
            "/api/v1/caches/{cache}/nars/{hash}/chunk",
            "/api/v1/caches/{cache}/webhooks/{webhook}",
            "/api/v1/user/keys/permissions",
        ] {
            assert!(api_key_cache_scope_allows_route(path).is_ok(), "{path}");
        }
    }

    #[test]
    fn a_cache_key_is_refused_outside_cache_routes() {
        for path in [
            "/api/v1/projects",
            "/api/v1/projects/{project}",
            "/api/v1/dashboard/tasks",
            "/api/v1/tasks/{project}/{task}/live",
            "/api/v1/user/stars/caches/{cache}",
            "/api/v1/caches-overview",
            "/api/v1/user",
        ] {
            assert!(
                matches!(
                    api_key_cache_scope_allows_route(path),
                    Err(AccessRefusal::CachePinnedKeyOnEndpoint)
                ),
                "{path}"
            );
        }
    }
}
