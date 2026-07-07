/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deserialized webhook payload shapes shared by the forge-hook handlers.

use serde::Deserialize;

// GitHub App installation / installation_repositories events.

#[derive(Deserialize)]
pub(super) struct GitHubInstallationPayload {
    pub(super) action: String,
    pub(super) installation: GitHubInstallation,
    pub(super) sender: Option<InstallationSender>,
    #[serde(default)]
    pub(super) repositories: Vec<GitHubRepoRef>,
    #[serde(default)]
    pub(super) repositories_added: Vec<GitHubRepoRef>,
}

impl GitHubInstallationPayload {
    /// Installed repos as lowercased `owner/repo`, merging the `installation`
    /// (`repositories`) and `installation_repositories` (`repositories_added`) shapes.
    pub(super) fn installed_full_names(&self) -> std::collections::HashSet<String> {
        self.repositories
            .iter()
            .chain(self.repositories_added.iter())
            .map(|r| r.full_name.to_ascii_lowercase())
            .collect()
    }
}

#[derive(Deserialize)]
pub(super) struct GitHubInstallation {
    pub(super) id: i64,
    pub(super) account: GitHubAccount,
}

#[derive(Deserialize)]
pub(super) struct GitHubAccount {
    pub(super) login: String,
}

#[derive(Deserialize)]
pub(super) struct GitHubRepoRef {
    pub(super) full_name: String,
}

#[derive(Deserialize)]
pub(super) struct InstallationSender {
    pub(super) login: String,
}

// GitHub check_run.requested_action events. `CheckRunSender` borrows from the
// request body, distinct from the owned `InstallationSender` above.

#[derive(Deserialize)]
pub(super) struct GithubCheckRunRequestedAction<'a> {
    pub(super) action: &'a str,
    pub(super) requested_action: Option<GithubRequestedAction<'a>>,
    pub(super) check_run: GithubCheckRunRef<'a>,
    pub(super) repository: GithubRepoFull<'a>,
    pub(super) sender: Option<CheckRunSender<'a>>,
}

#[derive(Deserialize)]
pub(super) struct GithubRequestedAction<'a> {
    pub(super) identifier: &'a str,
}

#[derive(Deserialize)]
pub(super) struct GithubCheckRunRef<'a> {
    pub(super) id: i64,
    #[serde(rename = "pull_requests", default)]
    _pull_requests: Vec<serde_json::Value>,
    #[serde(default)]
    _name: Option<&'a str>,
}

#[derive(Deserialize)]
pub(super) struct GithubRepoFull<'a> {
    pub(super) full_name: Option<&'a str>,
}

#[derive(Deserialize)]
pub(super) struct CheckRunSender<'a> {
    pub(super) login: &'a str,
}

// Issue/PR comment events (GitHub & Gitea) plus the GitLab Note Hook variant.

#[derive(Deserialize)]
pub(super) struct CommentPayload {
    #[serde(default)]
    pub(super) action: Option<String>,
    #[serde(default)]
    pub(super) comment: Option<CommentBody>,
    #[serde(default)]
    pub(super) issue: Option<CommentIssue>,
    #[serde(default)]
    pub(super) pull_request: Option<CommentIssue>,
    #[serde(default)]
    pub(super) sender: Option<CommentSender>,
    #[serde(default)]
    pub(super) repository: Option<CommentRepo>,
    #[serde(default)]
    pub(super) object_attributes: Option<GitlabNoteAttrs>,
    #[serde(default)]
    pub(super) user: Option<CommentSender>,
    #[serde(default)]
    pub(super) project: Option<GitlabNoteProject>,
    #[serde(default)]
    pub(super) merge_request: Option<GitlabNoteMr>,
}

#[derive(Deserialize, Default)]
pub(super) struct CommentBody {
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) id: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(super) struct CommentIssue {
    pub(super) number: Option<u64>,
}

#[derive(Deserialize, Default)]
pub(super) struct CommentSender {
    #[serde(default)]
    pub(super) login: Option<String>,
    #[serde(default)]
    pub(super) username: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct CommentRepo {
    #[serde(default)]
    pub(super) full_name: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct GitlabNoteAttrs {
    #[serde(default)]
    pub(super) id: Option<i64>,
    #[serde(default)]
    pub(super) note: Option<String>,
    #[serde(default)]
    pub(super) noteable_type: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct GitlabNoteProject {
    #[serde(default)]
    pub(super) path_with_namespace: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct GitlabNoteMr {
    #[serde(default)]
    pub(super) iid: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::GitHubInstallationPayload;

    #[test]
    fn installation_payload_collects_full_names_from_both_arrays() {
        let created: GitHubInstallationPayload = serde_json::from_value(serde_json::json!({
            "action": "created",
            "installation": { "id": 1, "account": { "login": "NuschtOS" } },
            "sender": { "login": "tester" },
            "repositories": [
                { "full_name": "NuschtOS/search" },
                { "full_name": "NuschtOS/nixos-modules" },
            ],
        }))
        .unwrap();
        let names = created.installed_full_names();
        assert!(names.contains("nuschtos/search"));
        assert!(names.contains("nuschtos/nixos-modules"));

        let added: GitHubInstallationPayload = serde_json::from_value(serde_json::json!({
            "action": "added",
            "installation": { "id": 1, "account": { "login": "NuschtOS" } },
            "sender": { "login": "tester" },
            "repositories_added": [ { "full_name": "NuschtOS/ixx" } ],
        }))
        .unwrap();
        assert!(added.installed_full_names().contains("nuschtos/ixx"));
    }
}
