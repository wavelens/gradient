/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Federate capability: traits for peers that relay/aggregate other peers'
//! work (i.e. gradient-proxy). In Spec 1 v1 these traits exist for the proxy
//! to implement; the gradient-server side is informational only.

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::GradientCapabilities;

#[async_trait]
pub trait FederateServer: Send + Sync {
    /// The peer's currently-aggregated capabilities. Called by the driver
    /// when the peer emits `AuthUpdate` so it can persist / re-advertise.
    async fn aggregated_capabilities(&self, peer_id: String) -> Result<GradientCapabilities>;
}

#[async_trait]
pub trait FederateClient: Send + Sync {
    /// Notification that a federate peer's aggregate capabilities changed.
    /// In v1 implementations typically just log; in Spec 2 they may write to
    /// the audit log or trigger re-scheduling.
    async fn on_aggregate_changed(
        &self,
        peer_id: String,
        new_caps: GradientCapabilities,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    #[async_trait]
    impl FederateServer for Noop {
        async fn aggregated_capabilities(&self, _: String) -> Result<GradientCapabilities> {
            Ok(GradientCapabilities::default())
        }
    }
    #[async_trait]
    impl FederateClient for Noop {
        async fn on_aggregate_changed(&self, _: String, _: GradientCapabilities) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn noop_drives() {
        let s: &dyn FederateServer = &Noop;
        let _ = s.aggregated_capabilities("p".into()).await.unwrap();

        let c: &dyn FederateClient = &Noop;
        c.on_aggregate_changed("p".into(), GradientCapabilities::default()).await.unwrap();
    }
}
