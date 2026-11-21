/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use gradient_git_host::reporter::CiStatus;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct RecordingReporter {
    calls: Mutex<Vec<String>>,
}

impl RecordingReporter {
    fn record(&self, method: &str, owner: &str, repo: &str) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{method} {owner}/{repo}"));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CiReporter for RecordingReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        self.record("report", &report.owner, &report.repo);
        Ok(None)
    }

    async fn add_reaction(&self, target: &ReactionTarget, _kind: ReactionKind) -> Result<()> {
        self.record("add_reaction", &target.owner, &target.repo);
        Ok(())
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        _branch: &str,
        _base: &str,
        _commit: &BranchCommit,
    ) -> Result<String> {
        self.record("upsert_branch", owner, repo);
        Ok(String::new())
    }

    async fn get_newest_undismissed_approval_review_commit_id_for_user(
        &self,
        owner: &str,
        repo: &str,
        _pr_number: u64,
        _user_id: i64,
        _webhook_sender_login: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.record(
            "get_newest_undismissed_approval_review_commit_id_for_user",
            owner,
            repo,
        );
        Ok(Some(vec![0xab; 20]))
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, _username: &str) -> Result<bool> {
        self.record("is_repo_writer", owner, repo);
        Ok(true)
    }
}

fn reporter_limited_to_acme_infra() -> (
    Arc<RecordingReporter>,
    ReporterRestrictedToGrantedRepository,
) {
    let recording = Arc::new(RecordingReporter::default());
    let limited = ReporterRestrictedToGrantedRepository::for_repository(
        recording.clone(),
        GitRepositoryOwnerAndName {
            owner: "acme".into(),
            name: "infra".into(),
        },
    );
    (recording, limited)
}

async fn call_report_reaction_and_branch(
    reporter: &ReporterRestrictedToGrantedRepository,
    owner: &str,
    repo: &str,
) -> [bool; 3] {
    let report = CiReport {
        owner: owner.into(),
        repo: repo.into(),
        sha: "abc".into(),
        context: "gradient/test".into(),
        status: CiStatus::Pending,
        description: None,
        details_url: None,
        existing_check_id: None,
        requested_actions: Vec::new(),
    };
    let target = ReactionTarget {
        owner: owner.into(),
        repo: repo.into(),
        pr_number: 1,
        comment_id: 1,
    };
    let commit = BranchCommit {
        message: "change".into(),
        author: None,
        files: Vec::new(),
    };
    [
        reporter.report(&report).await.is_ok(),
        reporter
            .add_reaction(&target, ReactionKind::ThumbsUp)
            .await
            .is_ok(),
        reporter
            .upsert_branch(owner, repo, "gradient/update", "main", &commit)
            .await
            .is_ok(),
    ]
}

#[tokio::test]
async fn refuses_report_reaction_and_branch_for_another_repository_of_the_same_owner() {
    let (recording, limited) = reporter_limited_to_acme_infra();
    assert_eq!(
        call_report_reaction_and_branch(&limited, "acme", "secrets").await,
        [false; 3]
    );
    assert!(recording.calls().is_empty());
}

#[tokio::test]
async fn refuses_report_reaction_and_branch_for_the_same_repository_name_of_another_owner() {
    let (recording, limited) = reporter_limited_to_acme_infra();
    assert_eq!(
        call_report_reaction_and_branch(&limited, "evil", "infra").await,
        [false; 3]
    );
    assert!(recording.calls().is_empty());
}

#[tokio::test]
async fn restricted_reporter_refuses_reviewed_commit_lookup_for_other_repository_and_forwards_for_granted_one()
 {
    let (recording, limited) = reporter_limited_to_acme_infra();
    for (owner, repo) in [("acme", "secrets"), ("evil", "infra")] {
        assert!(
            limited
                .get_newest_undismissed_approval_review_commit_id_for_user(
                    owner,
                    repo,
                    7,
                    42,
                    "maintainer"
                )
                .await
                .is_err(),
            "{owner}/{repo}"
        );
    }
    assert!(recording.calls().is_empty());

    let commit = limited
        .get_newest_undismissed_approval_review_commit_id_for_user(
            "Acme",
            "INFRA",
            7,
            42,
            "maintainer",
        )
        .await
        .unwrap();
    assert_eq!(commit, Some(vec![0xab; 20]));
    assert_eq!(
        recording.calls(),
        ["get_newest_undismissed_approval_review_commit_id_for_user Acme/INFRA"]
    );
}

#[tokio::test]
async fn restricted_reporter_refuses_repository_write_permission_check_for_other_owner_or_repository_without_forwarding_and_forwards_it_for_granted_repository_in_any_letter_case()
 {
    let (recording, limited) = reporter_limited_to_acme_infra();
    for (owner, repo) in [("acme", "secrets"), ("evil", "infra")] {
        assert!(
            limited
                .is_repo_writer(owner, repo, "maintainer")
                .await
                .is_err(),
            "{owner}/{repo}"
        );
    }
    assert!(recording.calls().is_empty());

    assert!(
        limited
            .is_repo_writer("Acme", "INFRA", "maintainer")
            .await
            .unwrap()
    );
    assert_eq!(recording.calls(), ["is_repo_writer Acme/INFRA"]);
}

#[tokio::test]
async fn forwards_report_reaction_and_branch_for_the_granted_repository_in_any_letter_case() {
    let (recording, limited) = reporter_limited_to_acme_infra();
    assert_eq!(
        call_report_reaction_and_branch(&limited, "Acme", "INFRA").await,
        [true; 3]
    );
    assert_eq!(
        recording.calls(),
        [
            "report Acme/INFRA",
            "add_reaction Acme/INFRA",
            "upsert_branch Acme/INFRA"
        ]
    );
}
