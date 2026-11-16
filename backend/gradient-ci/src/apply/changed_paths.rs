/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::triggers::ChangedFilePathFilter;
use gradient_util::glob::glob_match;

pub fn changed_paths_match_trigger_path_filter(
    filter: &ChangedFilePathFilter,
    changed_paths: &[String],
) -> bool {
    if filter.include_paths.is_empty() && filter.exclude_paths.is_empty() {
        return true;
    }
    changed_paths.iter().any(|path| {
        (filter.include_paths.is_empty() || path_matches_any_glob(&filter.include_paths, path))
            && !path_matches_any_glob(&filter.exclude_paths, path)
    })
}

fn path_matches_any_glob(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(include: &[&str], exclude: &[&str]) -> ChangedFilePathFilter {
        ChangedFilePathFilter {
            include_paths: include.iter().map(|p| p.to_string()).collect(),
            exclude_paths: exclude.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn paths(changed: &[&str]) -> Vec<String> {
        changed.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn empty_filter_passes_without_changed_paths() {
        assert!(changed_paths_match_trigger_path_filter(
            &filter(&[], &[]),
            &[]
        ));
    }

    #[test]
    fn include_only_needs_one_matching_path() {
        let f = filter(&["packages/kernel/**"], &[]);
        assert!(changed_paths_match_trigger_path_filter(
            &f,
            &paths(&["packages/other/x", "packages/kernel/a/b.nix"])
        ));
        assert!(!changed_paths_match_trigger_path_filter(
            &f,
            &paths(&["packages/other/x"])
        ));
    }

    #[test]
    fn exclude_only_needs_one_path_outside_the_exclude() {
        let f = filter(&[], &["docs/*"]);
        assert!(!changed_paths_match_trigger_path_filter(
            &f,
            &paths(&["docs/a.md", "docs/guides/b.md"])
        ));
        assert!(changed_paths_match_trigger_path_filter(
            &f,
            &paths(&["docs/a.md", "flake.nix"])
        ));
    }

    #[test]
    fn include_and_exclude_need_one_path_in_include_and_outside_exclude() {
        let f = filter(&["packages/*"], &["*.md"]);
        assert!(!changed_paths_match_trigger_path_filter(
            &f,
            &paths(&["packages/kernel/README.md", "flake.nix"])
        ));
        assert!(changed_paths_match_trigger_path_filter(
            &f,
            &paths(&["packages/kernel/README.md", "packages/kernel/default.nix"])
        ));
    }

    #[test]
    fn non_empty_filter_rejects_a_commit_without_changed_paths() {
        assert!(!changed_paths_match_trigger_path_filter(
            &filter(&[], &["docs/*"]),
            &[]
        ));
    }
}
