/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use gradient_db::WorkerDb;
use gradient_types::RuntimeConfig;
use gradient_wire::Rejection;
use gradient_wire::messages::{FailedPeer, GradientCapabilities};
use gradient_wire::traits::{AuthOutcome, DialerAuthority};

use super::session_authorization::{HandshakeProof, derive_session_authorization};
use crate::outbound::DialTarget;

#[derive(Clone, Debug)]
pub struct DialedSession {
    pub url: String,
    pub token_projects: Vec<String>,
    pub team_worker: bool,
}

impl DialTarget {
    pub(crate) fn session(&self) -> DialedSession {
        DialedSession {
            url: self.url.clone(),
            token_projects: self.token_projects.clone(),
            team_worker: self.team_worker,
        }
    }
}

pub(super) struct DialedWorkerAuthority {
    pub db: WorkerDb,
    pub config: Arc<RuntimeConfig>,
    pub session: DialedSession,
}

#[async_trait]
impl DialerAuthority for DialedWorkerAuthority {
    async fn admit(
        &self,
        worker_id: &str,
        advertised: GradientCapabilities,
    ) -> Result<AuthOutcome> {
        let proof = HandshakeProof::ServerDialed(self.session.clone());
        derive_session_authorization(&self.db, &self.config, worker_id, &proof, advertised)
            .await?
            .into_handshake_outcome()
    }
}

pub(super) fn rejection_of_dialed_worker(
    authorized: &[String],
    demoted: &[FailedPeer],
) -> Option<Rejection> {
    match (authorized.is_empty(), demoted.is_empty()) {
        (false, _) => None,
        (true, false) => Some(Rejection::NoCacheSubscribed),
        (true, true) => Some(Rejection::WorkerDeactivated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialed_worker_without_any_project_left_is_deactivated() {
        assert_eq!(
            rejection_of_dialed_worker(&[], &[]),
            Some(Rejection::WorkerDeactivated)
        );
    }

    #[test]
    fn a_dialed_worker_whose_projects_lack_a_cache_is_495() {
        let demoted = [FailedPeer {
            peer_id: "p1".into(),
            reason: "project has no cache subscribed".into(),
        }];
        assert_eq!(
            rejection_of_dialed_worker(&[], &demoted),
            Some(Rejection::NoCacheSubscribed)
        );
    }
}
