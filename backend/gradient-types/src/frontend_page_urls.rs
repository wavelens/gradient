/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use std::fmt::Display;

const NON_UNRESERVED_URL_BYTES_PERCENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn base(frontend_url: &str) -> &str {
    frontend_url.trim_end_matches('/')
}

pub fn project(frontend_url: &str, project: &str) -> String {
    format!("{}/project/{project}", base(frontend_url))
}

pub fn project_workers_page_url(frontend_url: &str, project: &str) -> String {
    format!("{}/project/{project}/workers", base(frontend_url))
}

pub fn task(frontend_url: &str, project: &str, task: &str) -> String {
    format!("{}/project/{project}/task/{task}", base(frontend_url))
}

pub fn evaluation(frontend_url: &str, project: &str, evaluation: impl Display) -> String {
    format!("{}/project/{project}/log/{evaluation}", base(frontend_url))
}

pub fn cache(frontend_url: &str, cache: &str) -> String {
    let cache = utf8_percent_encode(cache, NON_UNRESERVED_URL_BYTES_PERCENT_ENCODE_SET);
    format!("{}/caches/{cache}", base(frontend_url))
}

pub fn cached_store_path_page_url(frontend_url: &str, cache: &str, hash: &str) -> String {
    let hash = utf8_percent_encode(hash, NON_UNRESERVED_URL_BYTES_PERCENT_ENCODE_SET);
    format!("{}/nars?hash={hash}", self::cache(frontend_url, cache))
}

pub fn team(frontend_url: &str, team: &str) -> String {
    format!("{}/team/{team}", base(frontend_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls_follow_the_web_ui_routes() {
        let base = "https://ci.example/";
        assert_eq!(project(base, "p"), "https://ci.example/project/p");
        assert_eq!(task(base, "p", "t"), "https://ci.example/project/p/task/t");
        assert_eq!(
            project_workers_page_url(base, "p"),
            "https://ci.example/project/p/workers"
        );
        assert_eq!(
            evaluation(base, "p", "e"),
            "https://ci.example/project/p/log/e"
        );
        assert_eq!(cache(base, "c"), "https://ci.example/caches/c");
        assert_eq!(
            cached_store_path_page_url(base, "c", "h"),
            "https://ci.example/caches/c/nars?hash=h"
        );
        assert_eq!(team(base, "t"), "https://ci.example/team/t");
    }

    #[test]
    fn frontend_cache_page_urls_percent_encode_cache_name_and_hash() {
        assert_eq!(
            cache("https://ci.example", "//evil.example/\r\nx"),
            "https://ci.example/caches/%2F%2Fevil.example%2F%0D%0Ax"
        );
        assert_eq!(
            cached_store_path_page_url("https://ci.example", "c", "a&b=c#d"),
            "https://ci.example/caches/c/nars?hash=a%26b%3Dc%23d"
        );
    }
}
