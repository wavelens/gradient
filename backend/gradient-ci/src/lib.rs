/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod abort;
pub mod action_crypto;
pub mod actions;
pub mod apply;
pub mod context;
pub mod error;
pub mod git_host_connection_token_refresh;
pub mod github_app_connection;
pub mod github_app_manifest;
pub mod manifest_state;
pub mod pull_request_summary;
pub mod pull_requests;
pub mod reactions;
pub mod reporting;
pub mod task_commit_status_posting;
pub mod trigger;
pub mod unpark;
pub mod webhooks;

pub use self::abort::{AbortKind, abort_evaluation};
pub use self::apply::{
    ApplyError, ApplyInput, ApplyOutcome, ApprovalInfo, apply_trigger, park_if_no_cache,
    park_if_no_workers, park_if_pending_approval, park_storage_full,
};
pub use self::context::CiContext;
pub use self::error::ActionError;
pub use self::github_app_connection::*;
pub use self::reporting::*;
pub use self::trigger::*;
pub use self::unpark::{
    find_approval_gated_eval, set_evaluation_source_comment, unpark_approval,
    unpark_approval_with_wildcard, unpark_no_cache_for_project, unpark_no_workers_for_project,
    unpark_storage_full_all, unpark_storage_full_for_project,
};
pub use gradient_git_host::github_app::*;
pub use gradient_git_host::reporter::*;

/// A linker is dropping an rlib that nothing mentions, registry entries included. This call is
/// pulling the crate in for its `gradient_db::sql!` statements to reach the plan gate's registry.
pub const fn link() {}
