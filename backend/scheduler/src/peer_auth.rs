/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed peer-authorization mode for connected workers.
//!
//! A worker operates in one of two modes:
//!
//! - [`PeerAuth::Open`] — no peers have registered this worker; job candidates
//!   are not filtered by peer/org.
//! - [`PeerAuth::Restricted`] — one or more peer UUIDs have registered this
//!   worker; only jobs belonging to those peers are offered.
//!
//! Previously this was represented as `authorized_peers: HashSet<Uuid>` where
//! an empty set meant "open mode".  The explicit enum makes the intent
//! unambiguous at the call site and removes the need for `if p.is_empty()`
//! guards scattered across the scheduler.

use std::collections::HashSet;
use uuid::Uuid;

/// Peer authorization mode for a connected worker.
#[derive(Debug, Clone)]
pub enum PeerAuth {
    /// No peers registered — worker accepts jobs from all peers.
    Open,
    /// One or more peers registered — worker only sees jobs from these peers.
    Restricted(HashSet<Uuid>),
}

impl PeerAuth {
    /// Build a `PeerAuth` from a raw set of peer UUIDs.
    ///
    /// An empty set becomes [`PeerAuth::Open`]; a non-empty set becomes
    /// [`PeerAuth::Restricted`].
    pub fn from_peers(peers: HashSet<Uuid>) -> Self {
        if peers.is_empty() {
            Self::Open
        } else {
            Self::Restricted(peers)
        }
    }

    /// Returns `true` when the worker is in open mode (no peer filter).
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    /// Returns `true` when `id` is in the restricted set, or when the worker
    /// is in open mode (all peers are implicitly authorized).
    pub fn contains(&self, id: &Uuid) -> bool {
        match self {
            Self::Open => true,
            Self::Restricted(set) => set.contains(id),
        }
    }

    /// Returns the inner peer set for filtering job candidates, or `None` when
    /// the worker is in open mode (no filtering needed).
    pub fn as_filter(&self) -> Option<&HashSet<Uuid>> {
        match self {
            Self::Open => None,
            Self::Restricted(set) => Some(set),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_yields_open() {
        assert!(matches!(
            PeerAuth::from_peers(HashSet::new()),
            PeerAuth::Open
        ));
    }

    #[test]
    fn non_empty_set_yields_restricted() {
        let peer = Uuid::new_v4();
        assert!(matches!(
            PeerAuth::from_peers(HashSet::from([peer])),
            PeerAuth::Restricted(_)
        ));
    }

    #[test]
    fn open_contains_any_peer() {
        let auth = PeerAuth::Open;
        assert!(auth.contains(&Uuid::new_v4()));
    }

    #[test]
    fn restricted_contains_registered_peer() {
        let peer = Uuid::new_v4();
        let auth = PeerAuth::Restricted(HashSet::from([peer]));
        assert!(auth.contains(&peer));
    }

    #[test]
    fn restricted_does_not_contain_other_peer() {
        let peer = Uuid::new_v4();
        let other = Uuid::new_v4();
        let auth = PeerAuth::Restricted(HashSet::from([peer]));
        assert!(!auth.contains(&other));
    }

    #[test]
    fn open_as_filter_is_none() {
        assert!(PeerAuth::Open.as_filter().is_none());
    }

    #[test]
    fn restricted_as_filter_is_some() {
        let peer = Uuid::new_v4();
        let auth = PeerAuth::Restricted(HashSet::from([peer]));
        assert!(auth.as_filter().is_some());
    }
}
