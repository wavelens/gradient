/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Concurrency policies compare evaluations of a task with the same branch key only. Polls and
//! cron runs of the default branch use the branch name, like pushes to it. Pinned commits, build
//! requests, SSH sessions and input updates get `NO_BRANCH`, which policies skip.

pub const NO_BRANCH: &str = "none";

const BRANCH_REFS: &str = "refs/heads/";

pub fn branch(name: &str) -> String {
    format!("{BRANCH_REFS}{name}")
}

pub fn is_branch(key: &str) -> bool {
    key.starts_with(BRANCH_REFS)
}

pub fn tag(name: &str) -> String {
    format!("refs/tags/{name}")
}

pub fn pull_request(number: u64) -> String {
    format!("pull/{number}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_requests_tags_and_branches_of_one_name_get_distinct_keys() {
        let keys = [
            pull_request(1),
            pull_request(2),
            branch("1"),
            tag("1"),
            NO_BRANCH.to_owned(),
        ];
        let distinct: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len(), "{keys:?}");
    }
}
