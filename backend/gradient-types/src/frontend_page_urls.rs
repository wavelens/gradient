/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt::Display;

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

pub fn build_log(
    frontend_url: &str,
    project: &str,
    evaluation: impl Display,
    build: impl Display,
) -> String {
    format!(
        "{}?build={build}",
        self::evaluation(frontend_url, project, evaluation)
    )
}

pub fn cache(frontend_url: &str, cache: &str) -> String {
    format!("{}/caches/{cache}", base(frontend_url))
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
            build_log(base, "p", "e", "b"),
            "https://ci.example/project/p/log/e?build=b"
        );
        assert_eq!(cache(base, "c"), "https://ci.example/caches/c");
        assert_eq!(team(base, "t"), "https://ci.example/team/t");
    }
}
