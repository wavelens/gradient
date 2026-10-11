/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("pull_requests")]
#[derive(Args, Debug, Clone)]
pub struct PullRequestsArgs {
    /// Git author and committer name for commits of the `open_pr` action. Set it to `null` to
    /// use the default of the Git host.
    ///
    /// - GitHub: the App bot, with the commit marked as verified.
    /// - Gitea, Forgejo and GitLab: the token owner, with
    ///   `Gradient <gradient@users.noreply.HOST>` as fallback. Grant the token the `read:user`
    ///   or `read_user` scope for the owner lookup.
    pub commit_name: Option<String>,
    /// Git author and committer email for commits of the `open_pr` action. Set it to `null` to
    /// use the default of the Git host, as listed for
    /// {option}`services.gradient.pullRequests.commitName`.
    pub commit_email: Option<String>,
}
