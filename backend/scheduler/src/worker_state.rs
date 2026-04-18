/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Phantom-type states for connected workers.
//!
//! A [`TypedWorker<S>`] carries its lifecycle state in the type parameter `S`,
//! which is either [`Active`] or [`Draining`].  Methods that are invalid for
//! draining workers (notably capacity checks) are only available on
//! `TypedWorker<Active>`, so calling them on a draining worker is a
//! compile-time error rather than a runtime bug.
//!
//! Shared mutable data (architectures, assigned jobs, peer auth, …) lives in
//! [`WorkerShared`].  [`TypedWorker<S>`] implements `Deref<Target = WorkerShared>`
//! and `DerefMut`, so callers can access shared fields without an extra `.shared`
//! indirection.

use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;

use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

use gradient_core::types::proto::GradientCapabilities;

use crate::peer_auth::PeerAuth;

// ── Sealing trait ─────────────────────────────────────────────────────────────

mod private {
    pub trait Sealed {}
    impl Sealed for super::Active {}
    impl Sealed for super::Draining {}
}

/// Marker trait implemented by [`Active`] and [`Draining`].
///
/// Sealed — cannot be implemented outside this module.
pub trait WorkerMarker: private::Sealed + std::fmt::Debug + 'static {}

// ── State types ───────────────────────────────────────────────────────────────

/// The worker is active and eligible to receive new job offers.
#[derive(Debug)]
pub struct Active;

/// The worker is draining — it finishes in-flight jobs but accepts no new ones.
#[derive(Debug)]
pub struct Draining;

impl WorkerMarker for Active {}
impl WorkerMarker for Draining {}

// ── Shared worker data ────────────────────────────────────────────────────────

/// All fields that are relevant regardless of the worker's lifecycle state.
///
/// Accessed via [`TypedWorker<S>`]'s `Deref` / `DerefMut` impls.
#[derive(Debug)]
pub struct WorkerShared {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub assigned_jobs: HashSet<String>,
    /// Whether this worker operates in open (no peer filter) or restricted mode.
    pub peer_auth: PeerAuth,
    /// Job IDs already sent to this worker as candidates (for delta `JobOffer`).
    pub sent_candidates: HashSet<String>,
    /// Signalled by the API when registrations change and the worker should
    /// re-authenticate without disconnecting.
    pub reauth_notify: Arc<Notify>,
    /// Channel for sending abort messages to the handler for this worker.
    pub abort_tx: mpsc::UnboundedSender<(String, String)>,
}

// ── TypedWorker<S> ────────────────────────────────────────────────────────────

/// A connected worker whose lifecycle state is encoded in `S`.
///
/// Use [`TypedWorker::new_active`] to create an active worker; call
/// [`TypedWorker::<Active>::into_draining`] to transition it.
#[derive(Debug)]
pub struct TypedWorker<S: WorkerMarker> {
    pub(crate) shared: WorkerShared,
    _state: PhantomData<S>,
}

impl<S: WorkerMarker> std::ops::Deref for TypedWorker<S> {
    type Target = WorkerShared;
    fn deref(&self) -> &WorkerShared {
        &self.shared
    }
}

impl<S: WorkerMarker> std::ops::DerefMut for TypedWorker<S> {
    fn deref_mut(&mut self) -> &mut WorkerShared {
        &mut self.shared
    }
}

impl TypedWorker<Active> {
    /// Construct a new active worker.
    pub fn new(
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<Uuid>,
        reauth_notify: Arc<Notify>,
        abort_tx: mpsc::UnboundedSender<(String, String)>,
    ) -> Self {
        Self {
            shared: WorkerShared {
                capabilities,
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: 1,
                assigned_jobs: HashSet::new(),
                peer_auth: PeerAuth::from_peers(authorized_peers),
                sent_candidates: HashSet::new(),
                reauth_notify,
                abort_tx,
            },
            _state: PhantomData,
        }
    }

    /// Returns `true` when this worker can accept another build job.
    ///
    /// Only defined on `Active` — calling this on a draining worker is a
    /// compile-time error (draining workers never have build capacity).
    pub fn has_build_capacity(&self) -> bool {
        (self.assigned_jobs.len() as u32) < self.max_concurrent_builds
    }

    /// Consume this active worker and produce a draining one.
    ///
    /// The draining worker retains all in-flight assigned jobs but will not
    /// be offered new ones.
    pub fn into_draining(self) -> TypedWorker<Draining> {
        TypedWorker {
            shared: self.shared,
            _state: PhantomData,
        }
    }
}
