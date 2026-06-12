/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt as _;
use harmonia_store_path::StorePath;
use harmonia_store_remote::DaemonStore as _;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig, PooledConnectionGuard};
use harmonia_utils_hash::fmt::HashFormat as _;

use crate::commands::cache_upload::{UploadArgs, upload_one_owned};
use crate::narinfo::Narinfo;
use crate::output::{ExitKind, Output};

const DEFAULT_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

fn pool_config() -> PoolConfig {
    PoolConfig {
        max_size: 1,
        acquire_timeout: Duration::from_secs(600),
        ..Default::default()
    }
}

fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}

fn canonicalize(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/nix/store/{path}")
    }
}

struct ScopedGuard {
    inner: Option<PooledConnectionGuard>,
    ok: bool,
}

impl ScopedGuard {
    fn client(
        &mut self,
    ) -> &mut harmonia_store_remote::DaemonClient<
        tokio::net::unix::OwnedReadHalf,
        tokio::net::unix::OwnedWriteHalf,
    > {
        self.inner.as_mut().expect("guard already dropped").client()
    }

    fn mark_ok(&mut self) {
        self.ok = true;
    }
}

impl Drop for ScopedGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take()
            && !self.ok
        {
            inner.mark_broken();
        }
    }
}

async fn scoped(pool: &ConnectionPool) -> anyhow::Result<ScopedGuard> {
    let inner = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire daemon connection: {e}"))?;
    Ok(ScopedGuard {
        inner: Some(inner),
        ok: false,
    })
}

struct PathMeta {
    references: Vec<String>,
    deriver: Option<String>,
    nar_hash_sri: String,
}

async fn gather_path_meta(pool: &ConnectionPool, store_path: &str) -> anyhow::Result<PathMeta> {
    let sp = StorePath::from_base_path(strip_store_prefix(store_path))
        .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

    let mut guard = scoped(pool).await?;
    let pi = match guard.client().query_path_info(&sp).await {
        Ok(Some(pi)) => {
            guard.mark_ok();
            pi
        }
        Ok(None) => {
            guard.mark_ok();
            anyhow::bail!("path not in local store: {store_path}");
        }
        Err(e) => anyhow::bail!("query_path_info failed for {store_path}: {e}"),
    };

    let references: Vec<String> = pi
        .references
        .iter()
        .map(|r: &StorePath| {
            let s = r.to_string();
            s.strip_prefix("/nix/store/").unwrap_or(&s).to_owned()
        })
        .collect();

    Ok(PathMeta {
        references,
        deriver: pi.deriver.as_ref().map(|d| d.to_string()),
        nar_hash_sri: pi.nar_hash.as_sri().to_string(),
    })
}

async fn query_refs(pool: &ConnectionPool, store_path: &str) -> anyhow::Result<Vec<String>> {
    let sp = StorePath::from_base_path(strip_store_prefix(store_path))
        .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

    let mut guard = scoped(pool).await?;
    let pi = match guard.client().query_path_info(&sp).await {
        Ok(Some(pi)) => {
            guard.mark_ok();
            pi
        }
        Ok(None) => {
            guard.mark_ok();
            anyhow::bail!("path not in local store: {store_path}");
        }
        Err(e) => anyhow::bail!("query_path_info failed for {store_path}: {e}"),
    };

    Ok(pi
        .references
        .iter()
        .map(|r: &StorePath| canonicalize(&r.to_string()))
        .collect())
}

async fn runtime_closure(pool: &ConnectionPool, seeds: &[String]) -> HashSet<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = seeds.iter().map(|s| canonicalize(s)).collect();
    while let Some(path) = queue.pop_front() {
        if !visited.insert(path.clone()) {
            continue;
        }
        match query_refs(pool, &path).await {
            Ok(refs) => {
                for r in refs {
                    if !visited.contains(&r) {
                        queue.push_back(r);
                    }
                }
            }
            Err(e) => eprintln!("closure walk: skipping {path}: {e}"),
        }
    }
    visited
}

pub async fn upload_paths(args: &UploadArgs, out: Output) {
    let pool = ConnectionPool::new(DEFAULT_SOCKET, pool_config());

    let targets: Vec<String> = if args.full_closure {
        let seeds: Vec<String> = args.paths.iter().map(|p| canonicalize(p)).collect();
        let mut closure: Vec<String> = runtime_closure(&pool, &seeds).await.into_iter().collect();
        closure.sort();
        closure
    } else {
        args.paths.iter().map(|p| canonicalize(p)).collect()
    };

    for (i, store_path) in targets.iter().enumerate() {
        out.progress(format!("[{}/{}] uploading {store_path}", i + 1, targets.len()));
        let meta = match gather_path_meta(&pool, store_path).await {
            Ok(m) => m,
            Err(e) => out.err(ExitKind::Api, format!("metadata for {store_path}: {e}")),
        };

        let mut nar_stream =
            harmonia_file_nar::NarByteStream::new(PathBuf::from(store_path));
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk_result) = nar_stream.next().await {
            match chunk_result {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(e) => out.err(ExitKind::Api, format!("NAR stream error for {store_path}: {e}")),
            }
        }

        let file_size = bytes.len() as i64;
        let nar_size = bytes.len() as i64;

        let ni = Narinfo {
            store_path: store_path.clone(),
            url: None,
            file_hash: meta.nar_hash_sri.clone(),
            file_size,
            nar_hash: meta.nar_hash_sri,
            nar_size,
            references: meta.references,
            deriver: meta.deriver,
        };

        upload_one_owned(&args.cache, ni, bytes, out).await;
    }
}
