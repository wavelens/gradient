/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use gradient_types::ids::ProjectId;

#[derive(Debug, Clone)]
pub enum PeerAuth {
    Open,
    Restricted(HashSet<ProjectId>),
}

impl PeerAuth {
    pub fn from_peers(peers: HashSet<ProjectId>) -> Self {
        if peers.is_empty() {
            Self::Open
        } else {
            Self::Restricted(peers)
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    pub fn contains(&self, id: &ProjectId) -> bool {
        match self {
            Self::Open => true,
            Self::Restricted(set) => set.contains(id),
        }
    }

    pub fn as_filter(&self) -> Option<&HashSet<ProjectId>> {
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
        let peer = ProjectId::now_v7();
        assert!(matches!(
            PeerAuth::from_peers(HashSet::from([peer])),
            PeerAuth::Restricted(_)
        ));
    }

    #[test]
    fn restricted_does_not_contain_other_peer() {
        let peer = ProjectId::now_v7();
        let other = ProjectId::now_v7();
        let auth = PeerAuth::Restricted(HashSet::from([peer]));
        assert!(!auth.contains(&other));
    }
}
