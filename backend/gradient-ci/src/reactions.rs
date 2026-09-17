/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The thumbs-up/-down Gradient posts back on the `/gradient` PR comment that
//! triggered an evaluation, once that evaluation settles. Best-effort: the
//! reaction is decoration on a check that already reported.

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use tracing::warn;

use crate::actions::reporter_for_task;
use crate::context::CiContext;
use crate::{ReactionKind, ReactionTarget};

pub async fn react_to_source_comment_on_terminal(
    ctx: &CiContext,
    task_id: TaskId,
    evaluation: &MEvaluation,
    status: EvaluationStatus,
) {
    let kind = match status {
        EvaluationStatus::Completed => ReactionKind::ThumbsUp,
        EvaluationStatus::Failed | EvaluationStatus::Aborted => ReactionKind::ThumbsDown,
        _ => return,
    };
    let Some(raw) = evaluation.source_comment.as_ref() else {
        return;
    };
    // PR-triggered evals stamp `source_comment` with just `{pr_number, pr_author}`
    // (no `comment_id`) so the UI can show "PR #42"; there's no comment to react
    // to, so skip silently rather than warning about a "malformed" payload.
    if raw.get("comment_id").is_none() {
        return;
    }
    let Some(target) = parse_source_comment(raw) else {
        warn!(
            evaluation_id = %evaluation.id,
            "evaluation.source_comment present but malformed; skipping reaction"
        );
        return;
    };
    let reporter = match reporter_for_task(ctx, task_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %task_id, "resolving reporter for terminal-status reaction");
            return;
        }
    };
    if let Err(e) = reporter.add_reaction(&target, kind).await {
        warn!(error = %e, %task_id, ?kind, "/gradient terminal reaction post failed");
    }
}

fn parse_source_comment(value: &serde_json::Value) -> Option<ReactionTarget> {
    let owner = value.get("owner")?.as_str()?.to_string();
    let repo = value.get("repo")?.as_str()?.to_string();
    let pr_number = value.get("pr_number")?.as_u64()?;
    let comment_id = value.get("comment_id")?.as_i64()?;
    Some(ReactionTarget {
        owner,
        repo,
        pr_number,
        comment_id,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_source_comment;

    /// A PR-triggered eval stamps only `{pr_number, pr_author}`; there is no
    /// comment behind it, so the target must not parse.
    #[test]
    fn a_stamp_without_a_comment_is_not_a_reaction_target() {
        assert!(
            parse_source_comment(&serde_json::json!({"pr_number": 42, "pr_author": "a"})).is_none()
        );
        assert!(
            parse_source_comment(&serde_json::json!({
                "owner": "wavelens", "repo": "gradient", "pr_number": 42, "comment_id": 7
            }))
            .is_some()
        );
    }
}
