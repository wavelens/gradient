/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Forge integration layer: per-forge reporters, webhook parsing, signature
//! verification, and GitHub App auth, dispatched through one [`ForgeProvider`]
//! trait + [`ForgeRegistry`]. Adding a forge is a single `providers/*` impl plus
//! one [`ForgeRegistry::with_builtin`] registration.

pub mod git_push;
pub mod github_app;
pub mod pr;
pub mod provider;
pub mod providers;
pub mod registry;
pub mod reporter;
pub mod webhook;

pub use github_app::*;
pub use pr::{BranchCommit, CommitFile, PrRef};
pub use provider::ForgeProvider;
pub use registry::ForgeRegistry;
pub use reporter::*;
pub use webhook::{
    ParsedPullRequestEvent, ParsedPullRequestReviewEvent, ParsedPushEvent, ParsedReleaseEvent,
    PushCommit, PushOutcome, WebhookEventKind,
};
