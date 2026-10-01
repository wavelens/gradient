/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Git host integration layer: per-Git-host reporters, webhook parsing, signature
//! verification, and GitHub App auth, dispatched through one [`GitHostProvider`]
//! trait + [`GitHostRegistry`]. Adding a Git host is a single `providers/*` impl plus
//! one [`GitHostRegistry::with_builtin`] registration.

pub mod git_push;
pub mod github_app;
pub mod pr;
pub mod provider;
pub mod providers;
pub mod registry;
pub mod reporter;
pub mod webhook;

pub use github_app::*;
pub use pr::{BranchCommit, CommitFile, CommitIdent, PrRef};
pub use provider::GitHostProvider;
pub use registry::GitHostRegistry;
pub use reporter::*;
pub use webhook::{
    ParsedPullRequestEvent, ParsedPullRequestReviewEvent, ParsedPushEvent, ParsedReleaseEvent,
    PushCommit, PushOutcome, WebhookEventKind,
};
