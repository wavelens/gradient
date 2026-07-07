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
use harmonia_store_remote::UnkeyedValidPathInfo;
use harmonia_store_remote::pool::{ConnectionPool, PoolConfig};
use harmonia_utils_hash::Sha256;
use harmonia_utils_hash::fmt::HashFormat as _;

use crate::commands::cache_upload::{UploadArgs, upload_bytes};
use crate::narinfo::Narinfo;
use crate::output::{ExitKind, Output};

/// zstd level for uploaded NARs; matches the server's source NAR and the
/// worker's `NarPush`, so every object under `nars/` decompresses identically.
const NAR_ZSTD_LEVEL: i32 = 6;

const DEFAULT_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

fn pool_config() -> PoolConfig {
    PoolConfig {
        max_size: 1,
        connection_timeout: Duration::from_secs(600),
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

async fn query_path_info(
    pool: &ConnectionPool,
    store_path: &str,
) -> anyhow::Result<UnkeyedValidPathInfo> {
    let sp = StorePath::from_base_path(strip_store_prefix(store_path))
        .map_err(|e| anyhow::anyhow!("invalid store path {store_path}: {e}"))?;

    let mut guard = pool
        .acquire()
        .await
        .map_err(|e| anyhow::anyhow!("acquire daemon connection: {e}"))?;

    guard
        .execute(|client| async move { client.query_path_info(&sp).await })
        .await
        .map_err(|e| anyhow::anyhow!("query_path_info failed for {store_path}: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("path not in local store: {store_path}"))
}

struct PathMeta {
    references: Vec<String>,
    deriver: Option<String>,
    nar_hash_sri: String,
}

async fn gather_path_meta(pool: &ConnectionPool, store_path: &str) -> anyhow::Result<PathMeta> {
    let pi = query_path_info(pool, store_path).await?;

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
    let pi = query_path_info(pool, store_path).await?;

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

/// zstd-compress a raw NAR into the `.nar.zst` bytes the cache stores, returning
/// the compressed bytes and their `sha256:` SRI file hash (the narinfo
/// `FileHash`/`FileSize`). Uploading the raw NAR instead makes the worker's zstd
/// import fail with "Unknown frame descriptor".
fn compress_nar(nar_bytes: &[u8]) -> anyhow::Result<(Vec<u8>, String)> {
    let compressed = zstd::encode_all(std::io::Cursor::new(nar_bytes), NAR_ZSTD_LEVEL)?;
    let file_hash = Sha256::digest(&compressed).as_sri().to_string();
    Ok((compressed, file_hash))
}

pub async fn upload_paths(args: &UploadArgs, out: Output) {
    let pool = ConnectionPool::new(DEFAULT_SOCKET, pool_config());

    let targets: Vec<String> = if args.no_closure {
        args.paths.iter().map(|p| canonicalize(p)).collect()
    } else {
        let seeds: Vec<String> = args.paths.iter().map(|p| canonicalize(p)).collect();
        let mut closure: Vec<String> = runtime_closure(&pool, &seeds).await.into_iter().collect();
        closure.sort();
        closure
    };

    for (i, store_path) in targets.iter().enumerate() {
        let label = format!("[{}/{}]", i + 1, targets.len());
        out.step_start(format!("{label} Uploading {store_path}"));
        let meta = match gather_path_meta(&pool, store_path).await {
            Ok(m) => m,
            Err(e) => out.err(ExitKind::Api, format!("metadata for {store_path}: {e}")),
        };

        let mut nar_stream = harmonia_file_nar::NarByteStream::new(PathBuf::from(store_path));
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk_result) = nar_stream.next().await {
            match chunk_result {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(e) => out.err(
                    ExitKind::Api,
                    format!("NAR stream error for {store_path}: {e}"),
                ),
            }
        }

        let nar_size = bytes.len() as i64;
        let (compressed, file_hash) = match compress_nar(&bytes) {
            Ok(v) => v,
            Err(e) => out.err(
                ExitKind::Api,
                format!("compressing NAR for {store_path}: {e}"),
            ),
        };

        let ni = Narinfo {
            store_path: store_path.clone(),
            url: None,
            file_hash,
            file_size: compressed.len() as i64,
            nar_hash: meta.nar_hash_sri,
            nar_size,
            references: meta.references,
            deriver: meta.deriver,
        };

        upload_bytes(&args.cache, ni, compressed, out).await;
        out.step_done(format!("{label} Uploaded {store_path}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_nar_round_trips_and_hashes_compressed_bytes() {
        let raw = b"nar-payload-that-compresses-aaaaaaaaaaaaaaaaaaaaaaaa".repeat(8);
        let (compressed, file_hash) = compress_nar(&raw).expect("compress");

        assert_ne!(
            compressed, raw,
            "uploaded bytes must be zstd, not the raw NAR"
        );
        let round = zstd::decode_all(std::io::Cursor::new(&compressed)).expect("decode");
        assert_eq!(
            round, raw,
            "compressed bytes must decompress to the raw NAR"
        );

        let expected = Sha256::digest(&compressed).as_sri().to_string();
        assert_eq!(
            file_hash, expected,
            "FileHash must be sha256 of the compressed bytes"
        );
        assert!(file_hash.starts_with("sha256-"));
    }
}
