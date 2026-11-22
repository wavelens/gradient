/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ci_actions_do_not_contact_git_host_before_project_repository_grant::InstanceOwnedGitHostConnectionTestFixture;

#[tokio::test]
async fn open_pull_request_branch_pattern_dot_dot_segment_is_refused_records_refusal_and_git_host_receives_no_requests()
 {
    let setup = InstanceOwnedGitHostConnectionTestFixture::create().await;
    setup.insert_repository_grant_for_acme_infra().await;
    let action = setup
        .insert_open_pull_request_task_action_with_branch_pattern("gradient/../{input}")
        .await;
    let evaluation = setup.insert_commit_queued_evaluation_and_bumped_nixpkgs_flake_input_update_returning_evaluation_id().await;

    setup
        .run_open_pull_request_action_for_completed_evaluation_ignoring_result(&action, evaluation)
        .await;

    assert_eq!(setup.mock_git_host_received_http_request_count().await, 0);
    let errors = setup
        .all_recorded_task_action_delivery_error_messages()
        .await;
    assert!(
        matches!(errors.as_slice(), [Some(error)] if error.contains("The pull request branch gradient/../nixpkgs does not match the rules for git branch names")),
        "{errors:?}"
    );
    setup.database.finish().await;
}
