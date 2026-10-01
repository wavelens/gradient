/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct PullRequestsArgs {
    /// Author/committer name for commits the `OpenPr` action pushes. Unset (the
    /// default) lets each Git host choose: GitHub credits the App bot and marks
    /// the commit verified; Gitea, Forgejo and GitLab use the token owner.
    #[arg(
        long = "pull-requests-commit-name",
        env = "GRADIENT_PULL_REQUESTS_COMMIT_NAME"
    )]
    pub commit_name: Option<String>,
    /// Author/committer email for `OpenPr` commits; see `commit_name`.
    #[arg(
        long = "pull-requests-commit-email",
        env = "GRADIENT_PULL_REQUESTS_COMMIT_EMAIL"
    )]
    pub commit_email: Option<String>,
}
