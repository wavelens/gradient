/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Composition root for the running server. [`AppState`] holds every shared
//! handle as a flat field (so `gradient-web`/`proto`/`scheduler` keep
//! `state.<field>` access) and projects the per-layer [`StorageCtx`] /
//! [`DbContext`] / [`CiContext`] slices that `db` and `ci` functions take.
//! Nothing below this facade may name `AppState`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use crate::ci::CiContext;
use crate::ci::manifest_state::{ManifestStateStore, PendingCredentialsStore};
use crate::db::{DbContext, StatusReactor, WebDb, WorkerDb};
use crate::forge::ForgeRegistry;
use crate::shutdown::Shutdown;
use crate::state::{OidcGroupRoles, PendingOrgMemberships};
use crate::storage::{EmailSender, LogStorage, NarStore, StorageCtx};
use crate::types::{BoardEvent, RuntimeConfig, SecretString};

#[derive(Debug)]
pub struct AppState {
    /// Pool used by the proto handler, scheduler, cache GC, and any
    /// fire-and-forget background task spawned from a web handler that
    /// should not contend with foreground HTTP requests.
    pub worker_db: WorkerDb,
    /// Dedicated DB pool used by the axum/web layer so HTTP requests are
    /// not starved by the busy proto/scheduler pool under heavy NarPush load.
    pub web_db: WebDb,
    /// Resolved runtime configuration, built once at startup from the parsed
    /// [`crate::types::Cli`].
    pub config: Arc<RuntimeConfig>,
    pub log_storage: Arc<dyn LogStorage>,
    pub email: Arc<dyn EmailSender>,
    pub nar_storage: NarStore,
    /// Shared outbound HTTP client - reuse for any outbound request from a
    /// handler or background task; never construct a fresh `reqwest::Client`.
    pub http: reqwest::Client,
    /// Resolved-once registry of forge providers (reporters, webhook parsing,
    /// signature verification) shared into every [`CiContext`].
    pub forge: ForgeRegistry,
    /// Issued-but-unconsumed manifest CSRF state tokens with their issuance time.
    pub manifest_state: Arc<ManifestStateStore>,
    /// Manifest results awaiting one-shot pickup by the superuser's browser.
    pub pending_credentials: Arc<PendingCredentialsStore>,
    /// Graceful-shutdown coordination for all long-lived background tasks.
    pub shutdown: Shutdown,
    /// JWT signing/verification secret loaded once at startup.
    pub jwt_secret: SecretString,
    /// Wall-clock time the process bootstrapped; drives `gradient_uptime_seconds`.
    pub started_at: DateTime<Utc>,
    /// Org memberships declared in state for users who did not exist at apply
    /// time, drained per-username on first registration/OIDC login.
    pub pending_org_memberships: Arc<PendingOrgMemberships>,
    /// OIDC group -> (organization, role) grants resolved from state at startup.
    pub oidc_group_roles: Arc<OidcGroupRoles>,
    /// Broadcast of live board events to WebSocket subscribers.
    pub board_events: broadcast::Sender<BoardEvent>,
    /// Terminal-status reaction hook: `ci` turns terminal build/eval statuses
    /// into forge events and PR-comment reactions. Tests and worker-side flows
    /// use [`crate::db::NoReactor`].
    pub reactor: Arc<dyn StatusReactor>,
}

/// Kept as an alias so handler signatures and `Arc<ServerState>` call sites in
/// `gradient-web`/`proto`/`scheduler` stay unchanged. New code uses [`AppState`].
pub type ServerState = AppState;

impl AppState {
    /// Storage slice (cheap: clones a handful of `Arc`s / `Clone` handles).
    pub fn storage(&self) -> StorageCtx {
        StorageCtx {
            nar_storage: self.nar_storage.clone(),
            log_storage: self.log_storage.clone(),
            email: self.email.clone(),
        }
    }

    /// Db slice passed into `db::*` functions.
    pub fn db(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.clone(),
            web_db: self.web_db.clone(),
            config: self.config.clone(),
            storage: self.storage(),
            shutdown: self.shutdown.clone(),
            board_events: self.board_events.clone(),
            reactor: self.reactor.clone(),
        }
    }

    /// Ci slice passed into `ci::*` functions.
    pub fn ci(&self) -> CiContext {
        CiContext {
            db: self.db(),
            http: self.http.clone(),
            forge: self.forge.clone(),
        }
    }
}
