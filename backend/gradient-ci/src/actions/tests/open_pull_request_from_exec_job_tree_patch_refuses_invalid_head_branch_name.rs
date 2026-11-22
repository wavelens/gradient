/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::make_ctx_with;
use crate::ActionError;
use gradient_entity::exec_job_output;
use gradient_fixtures::mock::mock_db;
use gradient_types::{ExecJobId, GitHostConnectionId, TaskActionId, TaskId};
use gradient_wire::types::TREE_PATCH_OUTPUT;
use serde_json::json;
use std::io::Write as _;

async fn run_exec_tree_pr_with_tree_patch_output(entry_point_name: &str) -> anyhow::Result<()> {
    let job = ExecJobId::now_v7();
    let ctx = make_ctx_with(
        mock_db().append_query_results([vec![exec_job_output::Model {
            path: TREE_PATCH_OUTPUT.into(),
            size: 1,
            ..Default::default()
        }]]),
    );
    let mut staged = tempfile::NamedTempFile::new().expect("staged tree patch");
    staged
        .write_all(b"diff --git a/README b/README\n")
        .expect("write tree patch");
    ctx.db
        .storage
        .nar_storage
        .adopt_exec_output(job.into_inner(), TREE_PATCH_OUTPUT, staged.path())
        .await
        .expect("store tree patch");
    let payload = json!({ "job": job.to_string(), "entry_point_name": entry_point_name });

    super::super::send::execute_exec_tree_pr(
        &ctx,
        &payload,
        TaskActionId::now_v7(),
        TaskId::now_v7(),
        GitHostConnectionId::now_v7(),
        "gradient/{entry_point}",
        None,
        None,
        true,
    )
    .await
    .map(|_| ())
}

#[tokio::test]
async fn open_pull_request_branch_pattern_dot_dot_entry_point_is_refused_before_task_lookup_and_plain_entry_point_passes_branch_name_check()
 {
    let refused = run_exec_tree_pr_with_tree_patch_output("../web").await;
    assert!(
        matches!(
            refused.as_ref().map_err(|error| error.downcast_ref::<ActionError>()),
            Err(Some(ActionError::InvalidPullRequestBranchName { branch })) if branch == "gradient/../web"
        ),
        "{refused:?}"
    );

    let accepted = run_exec_tree_pr_with_tree_patch_output("web").await;
    assert!(
        accepted
            .as_ref()
            .is_err_and(|error| error.downcast_ref::<ActionError>().is_none()),
        "{accepted:?}"
    );
}
