/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Tracks which peers (orgs, caches, proxies) have registered a given worker ID
/// and holds the SHA-256 hash of the peer-issued token for challenge-response auth.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "worker_registration")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    /// The peer (org, cache, or proxy) that registered this worker.
    pub peer_id: Uuid,
    /// The persistent worker identity UUID sent in `InitConnection`.
    pub worker_id: String,
    /// SHA-256 hex digest of the token issued by the peer to this worker.
    pub token_hash: String,
    /// True when this row was created by declarative state management.
    pub managed: bool,
    /// WebSocket URL where the worker accepts incoming connections from the server.
    /// When set, the server connects outbound to this URL instead of waiting for
    /// the worker to connect inbound.
    pub url: Option<String>,
    /// When false, the server will refuse to authenticate this registration and
    /// will not dispatch jobs to this worker.
    pub active: bool,
    /// Optional human-readable display name for this worker (set server-side).
    pub name: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
