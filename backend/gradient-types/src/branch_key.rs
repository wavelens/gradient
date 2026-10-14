/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Concurrency policies compare evaluations of a task with the same branch key only. Pinned
//! commits, build requests, SSH sessions and input updates get `NO_BRANCH`, which policies skip.

pub const DEFAULT_BRANCH: &str = "";

pub const NO_BRANCH: &str = "none";

pub fn branch(name: &str) -> String {
    format!("refs/heads/{name}")
}

pub fn tag(name: &str) -> String {
    format!("refs/tags/{name}")
}

pub fn pull_request(number: u64) -> String {
    format!("pull/{number}")
}

pub fn polled(branch_name: Option<&str>) -> String {
    branch_name.map_or_else(|| DEFAULT_BRANCH.to_owned(), branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_and_a_poll_of_a_branch_share_the_key() {
        assert_eq!(polled(Some("main")), branch("main"));
        assert_eq!(branch("main"), "refs/heads/main");
    }

    #[test]
    fn a_poll_without_a_branch_follows_the_default_branch() {
        assert_eq!(polled(None), DEFAULT_BRANCH);
    }

    #[test]
    fn pull_requests_tags_and_branches_of_one_name_get_distinct_keys() {
        let keys = [
            pull_request(1),
            pull_request(2),
            branch("1"),
            tag("1"),
            DEFAULT_BRANCH.to_owned(),
            NO_BRANCH.to_owned(),
        ];
        let distinct: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{keys:?}");
    }
}
