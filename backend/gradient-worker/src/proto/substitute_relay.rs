/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Relay a build's outputs and their whole runtime closure from an upstream
//! cache straight into ours, without importing into the local nix store.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use gradient_proto::messages::{BuildSpec, CACHE_QUERY_MAX_PATHS, CachedPath, QueryMode};
use gradient_types::proto::JobPhase;
use sha2::{Digest as _, Sha256};
use tracing::debug;

use crate::proto::compression::{
    Compression, LEVEL6_WINDOW_BYTES, decompress, parse_nar_hash_to_bytes, resolve_compression,
    zstd_window_size,
};
use crate::proto::job::JobUpdater;
use crate::proto::prefetch::{
    CorruptCachedNar, MissingInputs, SubstituteNotOnUpstream, download_one_presigned,
};

/// The three cache operations the closure walk needs, behind a trait so the walk
/// itself is testable without a server on the other end of the socket.
pub(crate) trait RelayIo {
    /// Upstream availability plus narinfo (URL, hashes, references) per path.
    async fn pull(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>>;
    /// What our own cache already holds, and the PUT target for what it does not.
    async fn push_targets(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>>;
    /// Move one path's NAR into our cache, returning its references as the bare
    /// `hash-name` tokens the wire uses.
    async fn relay_one(
        &mut self,
        upstream: &CachedPath,
        target: Option<&CachedPath>,
    ) -> Result<Vec<String>>;
}

/// Mirror the runtime closure of `outputs` into our cache, breadth-first over the
/// upstream references, and return the outputs unchanged.
///
/// Relaying the outputs alone is what made a substituted anchor's closure absent
/// from our cache: the members below a pruned node have no anchor of their own, so
/// nothing else would ever fetch them, and every dependent's build fell back to the
/// upstream. Walking here is what lets `fetchable` mean "whole in OUR cache" (#593).
///
/// Each level asks Push what we already hold, Pull where the rest lives, relays the
/// rest and queues their references. A member no upstream serves is a
/// `SubstituteNotOnUpstream` miss, the same typed signal an output produces, so the
/// scheduler's handling is unchanged.
pub(crate) async fn relay_closure(
    io: &mut impl RelayIo,
    outputs: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = outputs.iter().map(|(_, p)| p.clone()).collect();

    while !frontier.is_empty() {
        let mut next: Vec<String> = Vec::new();
        for level in frontier.chunks(CACHE_QUERY_MAX_PATHS) {
            let level: Vec<String> = level
                .iter()
                .filter(|p| visited.insert((*p).clone()))
                .cloned()
                .collect();
            if level.is_empty() {
                continue;
            }

            let targets: HashMap<String, CachedPath> = io
                .push_targets(level.clone())
                .await?
                .into_iter()
                .map(|c| (c.path.clone(), c))
                .collect();
            let missing: Vec<String> = level
                .into_iter()
                .filter(|p| !targets.get(p).is_some_and(|c| c.cached))
                .collect();
            if missing.is_empty() {
                continue;
            }

            let upstream: HashMap<String, CachedPath> = io
                .pull(missing.clone())
                .await?
                .into_iter()
                .map(|c| (c.path.clone(), c))
                .collect();
            for path in missing {
                let up = upstream
                    .get(&path)
                    .filter(|c| c.cached && c.url.is_some())
                    .ok_or_else(|| anyhow::Error::new(SubstituteNotOnUpstream(path.clone())))?;
                let references = io.relay_one(up, targets.get(&path)).await?;
                next.extend(
                    references
                        .into_iter()
                        .map(|r| store_path(&r))
                        .filter(|r| *r != path),
                );
            }
        }
        frontier = next;
    }

    Ok(outputs.to_vec())
}

/// A reference as the wire carries it (`hash-name`, or occasionally a full path)
/// back to the full store path the cache queries take.
fn store_path(reference: &str) -> String {
    format!(
        "/nix/store/{}",
        reference.strip_prefix("/nix/store/").unwrap_or(reference)
    )
}

struct JobUpdaterIo<'a> {
    updater: &'a mut JobUpdater,
    drv_path: &'a str,
}

impl RelayIo for JobUpdaterIo<'_> {
    async fn pull(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>> {
        self.updater
            .query_cache(paths, QueryMode::Pull)
            .await
            .with_context(|| format!("CacheQuery Pull (substitute) for {}", self.drv_path))
    }

    async fn push_targets(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>> {
        self.updater
            .query_cache(paths, QueryMode::Push)
            .await
            .with_context(|| format!("CacheQuery Push (substitute) for {}", self.drv_path))
    }

    async fn relay_one(
        &mut self,
        upstream: &CachedPath,
        target: Option<&CachedPath>,
    ) -> Result<Vec<String>> {
        relay_one_path(self.updater, upstream, target).await
    }
}

/// Substitute a build's outputs and their runtime closure as a pure NAR relay.
/// Returns the output `(name, path)` pairs. Errors map to `SubstituteUnavailable`.
pub async fn relay_external_cached_outputs(
    task: &BuildSpec,
    updater: &mut JobUpdater,
) -> Result<Vec<(String, String)>> {
    let outputs: Vec<(String, String)> = task
        .outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect();
    if outputs.is_empty() {
        return Ok(Vec::new());
    }

    let mut io = JobUpdaterIo {
        updater,
        drv_path: &task.drv_path,
    };
    relay_closure(&mut io, &outputs).await
}

/// Download one upstream NAR, verify it, and push it into our cache, returning the
/// references it declares. Nothing enters the local nix store.
async fn relay_one_path(
    updater: &mut JobUpdater,
    upstream: &CachedPath,
    target: Option<&CachedPath>,
) -> Result<Vec<String>> {
    let path = &upstream.path;
    let fetch = updater.phase(JobPhase::SubstituteFetch);
    let (_, fetched) = download_one_presigned(crate::http::download_client(), upstream.clone())
        .await
        .with_context(|| format!("download upstream NAR for {path}"))?;
    drop(fetch);
    let (compressed, meta) = fetched.ok_or_else(|| {
        // The Pull reply said cached but the GET 404'd: the same typed self-heal
        // signal as a missing prefetch input, so the server can demote the stale
        // upstream record instead of retrying forever.
        anyhow::Error::new(MissingInputs(vec![path.clone()])).context(format!(
            "upstream reported {path} but the NAR object is missing"
        ))
    })?;

    let kind = resolve_compression(&compressed, meta.url.as_deref());

    // Upstream references arrive as full /nix/store paths; NarUploaded wants
    // hash-name tokens.
    let references: Vec<String> = meta
        .references
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            r.strip_prefix("/nix/store/")
                .unwrap_or(r.as_str())
                .to_string()
        })
        .collect();

    // Pure relay: the upstream NAR is already zstd with a window at our level-6
    // threshold and carries the file/nar metadata, so store the bytes verbatim -
    // no decompress, no recompress, no rehash.
    let verbatim = (kind == Compression::Zstd
        && zstd_window_size(&compressed).is_some_and(|w| w >= LEVEL6_WINDOW_BYTES))
    .then(|| {
        Some((
            meta.file_hash.clone()?,
            meta.nar_hash.clone()?,
            meta.nar_size?,
        ))
    })
    .flatten();

    let (bytes, cmeta) = if let Some((file_hash, nar_hash, nar_size)) = verbatim {
        let file_size = compressed.len() as u64;
        (
            compressed,
            crate::proto::nar::CompressedNarMeta {
                file_hash,
                file_size,
                nar_hash,
                nar_size,
            },
        )
    } else {
        // Weaker/absent upstream compression: decompress (verifying against the
        // upstream nar_hash) and recompress at our level-6 threshold. Multi-MB CPU
        // work, so it runs on the blocking pool.
        let mut compress = updater.phase(JobPhase::Compress);
        compress.record(1, compressed.len() as u64);
        let claimed = meta.nar_hash.clone();
        let p = path.clone();
        tokio::task::spawn_blocking(move || {
            let raw = decompress(&compressed, kind)
                .with_context(|| format!("{kind:?} decompress for {p}"))?;
            if let Some(claimed) = claimed.as_deref() {
                let actual: [u8; 32] = Sha256::digest(&raw).into();
                let want = parse_nar_hash_to_bytes(claimed)
                    .with_context(|| format!("invalid upstream nar_hash for {p}"))?;
                if actual != want {
                    return Err(anyhow::Error::new(CorruptCachedNar(p.clone()))
                        .context(format!("upstream NAR hash mismatch for {p}")));
                }
            }
            crate::proto::nar::compress_nar(&raw)
                .with_context(|| format!("recompress relay NAR for {p}"))
        })
        .await
        .context("relay decompress task panicked")??
    };

    // Transport: S3-backed caches expose a presigned PUT URL; local-disk caches
    // return none and accept the bytes via direct NarPush frames.
    let mut push = updater.phase(JobPhase::NarPush);
    push.record(1, bytes.len() as u64);
    crate::proto::nar::upload_nar(
        &updater.job_id,
        path,
        crate::proto::nar::NarSource::Compressed {
            bytes: &bytes,
            meta: cmeta,
            references: references.clone(),
            deriver: meta.deriver.clone(),
            ca: upstream.ca.clone(),
        },
        crate::proto::nar::NarSink::from_upload_url(
            target.and_then(|c| c.url.as_deref()),
            &updater.nar_recv,
        ),
        &updater.writer,
    )
    .await
    .with_context(|| format!("relay-push {path} into our cache"))?;

    debug!(%path, "relayed substitute NAR into our cache");
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// An upstream of `path: references`; Push reports `cached` for everything in
    /// `have`; every relayed path is recorded in order.
    struct Fake {
        upstream: BTreeMap<String, Vec<String>>,
        have: BTreeSet<String>,
        relayed: Vec<String>,
    }

    fn cp(path: &str, cached: bool) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached,
            url: cached.then(|| format!("https://up{path}")),
            ..Default::default()
        }
    }

    impl RelayIo for Fake {
        async fn pull(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>> {
            Ok(paths
                .iter()
                .map(|p| cp(p, self.upstream.contains_key(p)))
                .collect())
        }

        async fn push_targets(&mut self, paths: Vec<String>) -> Result<Vec<CachedPath>> {
            Ok(paths.iter().map(|p| cp(p, self.have.contains(p))).collect())
        }

        async fn relay_one(
            &mut self,
            upstream: &CachedPath,
            _target: Option<&CachedPath>,
        ) -> Result<Vec<String>> {
            self.relayed.push(upstream.path.clone());
            self.have.insert(upstream.path.clone());
            Ok(self.upstream[&upstream.path].clone())
        }
    }

    fn fake(edges: &[(&str, &[&str])], have: &[&str]) -> Fake {
        Fake {
            upstream: edges
                .iter()
                .map(|(p, r)| {
                    (
                        (*p).to_string(),
                        r.iter().map(|s| (*s).to_string()).collect(),
                    )
                })
                .collect(),
            have: have.iter().map(|s| (*s).to_string()).collect(),
            relayed: Vec::new(),
        }
    }

    fn out(path: &str) -> Vec<(String, String)> {
        vec![("out".to_owned(), path.to_owned())]
    }

    /// The whole point: every member of the runtime closure our cache lacks is
    /// relayed, exactly once even on a diamond, and a member we already hold is
    /// neither fetched nor walked through.
    #[tokio::test]
    async fn relays_every_missing_member_once_and_skips_what_we_have() {
        let mut io = fake(
            &[
                ("/nix/store/aaa-out", &["bbb-lib", "ccc-lib"]),
                ("/nix/store/bbb-lib", &["ccc-lib"]),
                ("/nix/store/ccc-lib", &["ccc-lib"]),
            ],
            &["/nix/store/ccc-lib"],
        );

        let outputs = relay_closure(&mut io, &out("/nix/store/aaa-out"))
            .await
            .unwrap();

        assert_eq!(outputs, out("/nix/store/aaa-out"));
        assert_eq!(io.relayed, ["/nix/store/aaa-out", "/nix/store/bbb-lib"]);
    }

    /// A closure member no upstream serves is the same typed miss an output is, so
    /// the scheduler counts it against the same budget.
    #[tokio::test]
    async fn a_member_no_upstream_serves_is_a_substitute_miss() {
        let mut io = fake(&[("/nix/store/aaa-out", &["bbb-lib"])], &[]);

        let err = relay_closure(&mut io, &out("/nix/store/aaa-out"))
            .await
            .unwrap_err();

        assert!(
            err.downcast_ref::<SubstituteNotOnUpstream>()
                .is_some_and(|m| m.0 == "/nix/store/bbb-lib"),
            "{err:#}"
        );
    }

    /// An output already whole in our cache costs nothing: no download, and no walk
    /// of a closure we must already hold for it to be whole.
    #[tokio::test]
    async fn an_output_already_in_our_cache_is_not_walked() {
        let mut io = fake(
            &[("/nix/store/aaa-out", &["bbb-lib"])],
            &["/nix/store/aaa-out"],
        );

        relay_closure(&mut io, &out("/nix/store/aaa-out"))
            .await
            .unwrap();

        assert!(io.relayed.is_empty());
    }

    /// A self-reference is the common case for a store path and must not queue the
    /// path that just landed back onto the frontier.
    #[tokio::test]
    async fn a_self_reference_terminates() {
        let mut io = fake(&[("/nix/store/aaa-out", &["aaa-out"])], &[]);

        relay_closure(&mut io, &out("/nix/store/aaa-out"))
            .await
            .unwrap();

        assert_eq!(io.relayed, ["/nix/store/aaa-out"]);
    }
}
