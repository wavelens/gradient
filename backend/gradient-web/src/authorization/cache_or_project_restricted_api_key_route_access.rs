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
const ALL_PROJECTS_ROUTE_PREFIXES: &[&str] = &[
    "/dashboard",
    "/search",
    "/board",
    "/metrics/catalog",
    "/metrics/query",
    "/metrics/events",
];

fn api_route(matched_path: &str) -> &str {
    matched_path.strip_prefix("/api/v1").unwrap_or(matched_path)
}

fn is_under(route: &str, prefix: &str) -> bool {
    route
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

pub(crate) fn api_key_cache_scope_allows_route(matched_path: &str) -> Result<(), AccessRefusal> {
    let route = api_route(matched_path);
    if is_under(route, CACHE_KEY_PREFIX) || CACHE_KEY_ROUTES.contains(&route) {
        Ok(())
    } else {
        Err(AccessRefusal::CachePinnedKeyOnEndpoint)
    }
}

pub(crate) fn reject_api_key_for_route_reading_all_projects(
    matched_path: &str,
) -> Result<(), AccessRefusal> {
    let route = api_route(matched_path);
    if ALL_PROJECTS_ROUTE_PREFIXES
        .iter()
        .any(|prefix| is_under(route, prefix))
    {
        Err(AccessRefusal::ProjectPinnedKeyOnAllProjectsEndpoint)
    } else {
        Ok(())
    }
}

pub(crate) fn refuse_request_outside_api_key_scope(
    parts: &Parts,
    api_key: Option<&ApiKeyContext>,
) -> Result<(), AccessRefusal> {
    let Some(key) = api_key else {
        return Ok(());
    };
    let matched_path = parts
        .extensions
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    if key.cache_pin.is_some() {
        api_key_cache_scope_allows_route(
            matched_path.ok_or(AccessRefusal::CachePinnedKeyOnEndpoint)?,
        )?;
    }
    if key.project.is_some() {
        reject_api_key_for_route_reading_all_projects(
            matched_path.ok_or(AccessRefusal::ProjectPinnedKeyOnAllProjectsEndpoint)?,
        )?;
    }
    Ok(())
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

    #[test]
    fn a_project_key_is_refused_on_routes_reading_all_projects() {
        for path in [
            "/api/v1/dashboard/tasks",
            "/api/v1/dashboard/rail",
            "/api/v1/search",
            "/api/v1/board/jobs/pending",
            "/api/v1/board/jobs/{id}/abort",
            "/api/v1/metrics/catalog",
            "/api/v1/metrics/query",
            "/api/v1/metrics/events",
        ] {
            assert!(
                matches!(
                    reject_api_key_for_route_reading_all_projects(path),
                    Err(AccessRefusal::ProjectPinnedKeyOnAllProjectsEndpoint)
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn a_project_key_reaches_project_routes() {
        for path in [
            "/api/v1/projects",
            "/api/v1/projects/{project}",
            "/api/v1/metrics/tasks/{project}/{task}/evaluations",
            "/api/v1/tasks/{project}/{task}/live",
            "/api/v1/user/search",
            "/api/v1/boards",
        ] {
            assert!(
                reject_api_key_for_route_reading_all_projects(path).is_ok(),
                "{path}"
            );
        }
    }
}
