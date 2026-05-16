/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fetch capability: traits for retrieving flake sources and store paths
//! from configured remotes (git/https/etc.).

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait FetchServer: Send + Sync {
    /// Resolve a git/https flake source URL into a cached store path.
    /// Returns the resulting store path on success.
    async fn fetch_flake(&self, url: String, commit: String) -> Result<String>;
}

#[async_trait]
pub trait FetchClient: Send + Sync {
    /// Called when a peer reports a completed fetch with the resulting store
    /// path. The client may persist the cache entry, archive the source, etc.
    async fn on_fetch_result(
        &self,
        peer_id: String,
        job_id: String,
        flake_source: Option<String>,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    #[async_trait]
    impl FetchServer for Noop {
        async fn fetch_flake(&self, _: String, _: String) -> Result<String> {
            Ok("/nix/store/x".into())
        }
    }
    #[async_trait]
    impl FetchClient for Noop {
        async fn on_fetch_result(
            &self,
            _: String,
            _: String,
            _: Option<String>,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn noop_drives() {
        let s: &dyn FetchServer = &Noop;
        let p = s.fetch_flake("g".into(), "c".into()).await.unwrap();
        assert!(!p.is_empty());

        let c: &dyn FetchClient = &Noop;
        c.on_fetch_result("p".into(), "j".into(), None).await.unwrap();
    }
}
