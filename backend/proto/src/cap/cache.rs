/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Cache capability: NAR/narinfo query and transfer traits.

use anyhow::Result;
use async_trait::async_trait;

use crate::messages::{CachedPath, QueryMode};

#[async_trait]
pub trait CacheServer: Send + Sync {
    /// Answer a `CacheQuery` for the given paths. Returns one `CachedPath`
    /// entry per input path with cache status (and optional presigned URLs,
    /// depending on `mode`).
    async fn query_paths(
        &self,
        peer_id: String,
        paths: Vec<String>,
        mode: QueryMode,
    ) -> Result<Vec<CachedPath>>;

    /// Stream a NAR for `path_hash` to the requesting peer.
    /// Implementations return the path of the NAR file the driver should
    /// stream (or `None` if the cache does not have it).
    async fn read_nar(
        &self,
        peer_id: String,
        path_hash: String,
    ) -> Result<Option<std::path::PathBuf>>;

    /// Accept an incoming NAR push for `path_hash`. Implementations write
    /// the streamed bytes to durable storage and return on completion.
    async fn write_nar(&self, peer_id: String, path_hash: String, bytes: Vec<u8>) -> Result<()>;
}

#[async_trait]
pub trait CacheClient: Send + Sync {
    /// Called when an upstream cache server confirms it has stored a pushed NAR.
    async fn on_cache_status(&self, peer_id: String, path_hash: String, cached: bool)
    -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    #[async_trait]
    impl CacheServer for Noop {
        async fn query_paths(
            &self,
            _: String,
            _: Vec<String>,
            _: QueryMode,
        ) -> Result<Vec<CachedPath>> {
            Ok(vec![])
        }
        async fn read_nar(&self, _: String, _: String) -> Result<Option<std::path::PathBuf>> {
            Ok(None)
        }
        async fn write_nar(&self, _: String, _: String, _: Vec<u8>) -> Result<()> {
            Ok(())
        }
    }
    #[async_trait]
    impl CacheClient for Noop {
        async fn on_cache_status(&self, _: String, _: String, _: bool) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn noop_drives() {
        let s: &dyn CacheServer = &Noop;
        let _ = s
            .query_paths("p".into(), vec![], QueryMode::Normal)
            .await
            .unwrap();
        let _ = s.read_nar("p".into(), "h".into()).await.unwrap();
        s.write_nar("p".into(), "h".into(), vec![]).await.unwrap();

        let c: &dyn CacheClient = &Noop;
        c.on_cache_status("p".into(), "h".into(), true)
            .await
            .unwrap();
    }
}
