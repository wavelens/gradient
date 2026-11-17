/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::DbContext;
use gradient_db::lookup::by_id;
use gradient_sources::{ComparisonBaseCommit, get_changed_file_path_pairs_between_commits};
use gradient_types::triggers::ChangedFilePathFilter;
use gradient_types::*;
use gradient_util::glob::glob_match;
use sea_orm::ConnectionTrait;
use std::time::Duration;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathFilterBaseCommit {
    NewestEvaluationOnBranch(String),
    TargetBranchTip(String),
    CommitHashBytes(Vec<u8>),
}

pub struct PathFilterCommitComparison<'a> {
    pub base: &'a PathFilterBaseCommit,
    pub head: &'a [u8],
    pub head_repository: Option<&'a str>,
}

pub async fn should_fire_trigger_for_path_filter<C: ConnectionTrait>(
    db: &C,
    sources: &DbContext,
    task: &MTask,
    filter: &ChangedFilePathFilter,
    comparison: PathFilterCommitComparison<'_>,
    budget: Duration,
) -> bool {
    if path_filter_has_no_include_or_exclude_patterns(filter) {
        return true;
    }
    let changed = changed_paths_between_base_and_head(db, sources, task, comparison);
    match tokio::time::timeout(budget, changed).await {
        Ok(Ok(Some(paths))) => changed_paths_match_trigger_path_filter(filter, &paths),
        Ok(Ok(None)) => true,
        Ok(Err(error)) => {
            warn!(%error, task_id = %task.id, "Failed to get the changed file list for the trigger path filter. Trigger fires anyway.");
            true
        }
        Err(_) => {
            warn!(task_id = %task.id, budget_secs = budget.as_secs(), "Listing changed files for path filter of trigger timed out. Run trigger anyway.");
            true
        }
    }
}

async fn changed_paths_between_base_and_head<C: ConnectionTrait>(
    db: &C,
    sources: &DbContext,
    task: &MTask,
    comparison: PathFilterCommitComparison<'_>,
) -> anyhow::Result<Option<Vec<String>>> {
    let base = match comparison.base {
        PathFilterBaseCommit::NewestEvaluationOnBranch(branch) => {
            match find_newest_task_branch_evaluation_commit_hash_bytes(db, task.id, branch).await? {
                Some(hash) if hash != comparison.head => {
                    ComparisonBaseCommit::CommitHashBytes(hash)
                }
                _ => return Ok(None),
            }
        }
        PathFilterBaseCommit::TargetBranchTip(branch) => {
            ComparisonBaseCommit::BranchName(branch.clone())
        }
        PathFilterBaseCommit::CommitHashBytes(hash) if hash != comparison.head => {
            ComparisonBaseCommit::CommitHashBytes(hash.clone())
        }
        PathFilterBaseCommit::CommitHashBytes(_) => return Ok(None),
    };
    let paths = get_changed_file_path_pairs_between_commits(
        sources,
        task,
        base,
        comparison.head,
        comparison.head_repository,
    )
    .await?;
    Ok(Some(paths))
}

async fn find_newest_task_branch_evaluation_commit_hash_bytes<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    branch: &str,
) -> Result<Option<Vec<u8>>, sea_orm::DbErr> {
    let Some(newest) = gradient_db::evaluations::branch::newest_on_branch(db, task, branch).await?
    else {
        return Ok(None);
    };
    Ok(by_id::<ECommit, _>(db, newest.commit)
        .await?
        .map(|commit| commit.hash))
}

fn path_filter_has_no_include_or_exclude_patterns(filter: &ChangedFilePathFilter) -> bool {
    filter.include_paths.is_empty() && filter.exclude_paths.is_empty()
}

pub fn changed_paths_match_trigger_path_filter(
    filter: &ChangedFilePathFilter,
    changed_paths: &[String],
) -> bool {
    if path_filter_has_no_include_or_exclude_patterns(filter) {
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
