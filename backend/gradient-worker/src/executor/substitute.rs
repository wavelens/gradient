/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A Substitute spec: each output's NAR straight from an upstream cache into ours,
//! repacked on the way. Nothing enters the local store and nothing below an output
//! is fetched; the server demands the producers of what the NAR references.

use std::collections::HashSet;

use anyhow::{Context, Result};
use gradient_proto::messages::{CachedPath, JobPhase};
use gradient_util::nix_hash::normalize_nar_hash;

use crate::proto::compression::{decompress, resolve_compression};
use crate::proto::job::JobUpdater;
use crate::proto::nar::sha256_nix32;
use crate::proto::prefetch::{
    CorruptCachedNar, MissingInputs, SubstituteNotOnUpstream, download_one_presigned,
};
use crate::proto::progress::{Progress, ProgressSink};

#[derive(Debug)]
pub(crate) struct RawNar {
    pub nar: Vec<u8>,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
}

#[derive(Debug)]
pub(crate) struct FetchedOutput {
    pub name: String,
    pub store_path: String,
    /// `None`: our cache already had it, so there is nothing to push.
    pub nar: Option<RawNar>,
}

/// The three cache operations a fetch needs, behind a trait so the fetch itself is
/// testable without a server on the other end of the socket.
pub(crate) trait UpstreamIo {
    async fn have(&mut self, paths: Vec<String>) -> Result<HashSet<String>>;
    async fn locate(&mut self, path: &str) -> Result<Option<CachedPath>>;
    async fn download(
        &mut self,
        upstream: &CachedPath,
        progress: &mut Progress<impl ProgressSink>,
    ) -> Result<Option<Vec<u8>>>;
}

pub(crate) async fn fetch_outputs(
    io: &mut impl UpstreamIo,
    outputs: &[(String, String)],
    progress: &mut Progress<impl ProgressSink>,
) -> Result<Vec<FetchedOutput>> {
    let located = locate_missing(io, outputs).await?;
    progress.set_total(download_size(
        located.iter().filter_map(|(_, _, u)| u.as_ref()),
    ));

    let mut fetched = Vec::with_capacity(located.len());
    for (name, store_path, upstream) in located {
        let nar = match upstream {
            Some(upstream) => Some(fetch_one(io, &upstream, progress).await?),
            None => None,
        };
        fetched.push(FetchedOutput {
            name,
            store_path,
            nar,
        });
    }
    if fetched.iter().any(|f| f.nar.is_some()) {
        progress.finish().await;
    }

    Ok(fetched)
}

/// Every output paired with the upstream entry to fetch it from; `None` for an
/// output our cache already holds. Fails before any byte moves when one is on
/// no upstream.
async fn locate_missing(
    io: &mut impl UpstreamIo,
    outputs: &[(String, String)],
) -> Result<Vec<(String, String, Option<CachedPath>)>> {
    let have = io
        .have(outputs.iter().map(|(_, path)| path.clone()).collect())
        .await?;
    let mut located = Vec::with_capacity(outputs.len());
    for (name, path) in outputs {
        let upstream = if have.contains(path) {
            None
        } else {
            Some(
                io.locate(path)
                    .await?
                    .ok_or_else(|| anyhow::Error::new(SubstituteNotOnUpstream(path.clone())))?,
            )
        };
        located.push((name.clone(), path.clone(), upstream));
    }

    Ok(located)
}

/// The compressed bytes to move, known only when every upstream declared its size.
fn download_size<'a>(upstreams: impl Iterator<Item = &'a CachedPath>) -> Option<u64> {
    upstreams.map(|u| u.file_size).sum()
}

async fn fetch_one(
    io: &mut impl UpstreamIo,
    upstream: &CachedPath,
    progress: &mut Progress<impl ProgressSink>,
) -> Result<RawNar> {
    let path = &upstream.path;
    let compressed = io
        .download(upstream, progress)
        .await?
        .ok_or_else(|| anyhow::Error::new(MissingInputs(vec![path.clone()])))?;
    progress.transfer_done();
    let nar = decompress(
        &compressed,
        resolve_compression(&compressed, upstream.url.as_deref()),
    )
    .with_context(|| format!("decompress the upstream NAR of {path}"))?;
    if upstream
        .nar_hash
        .as_deref()
        .is_some_and(|claimed| normalize_nar_hash(claimed) != sha256_nix32(&nar))
    {
        return Err(anyhow::Error::new(CorruptCachedNar(path.clone())));
    }

    let references = upstream
        .references
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.trim_start_matches("/nix/store/").to_owned())
        .collect();

    Ok(RawNar {
        nar,
        references,
        deriver: upstream.deriver.clone(),
        ca: upstream.ca.clone(),
    })
}

pub(crate) struct JobUpdaterIo<'a>(pub &'a mut JobUpdater);

impl UpstreamIo for JobUpdaterIo<'_> {
    async fn have(&mut self, paths: Vec<String>) -> Result<HashSet<String>> {
        let sizes = vec![None; paths.len()];
        Ok(self
            .0
            .query_push(paths, sizes)
            .await?
            .into_iter()
            .filter(|cp| cp.cached)
            .map(|cp| cp.path)
            .collect())
    }

    async fn locate(&mut self, path: &str) -> Result<Option<CachedPath>> {
        self.0.query_upstream(path.to_owned()).await
    }

    async fn download(
        &mut self,
        upstream: &CachedPath,
        progress: &mut Progress<impl ProgressSink>,
    ) -> Result<Option<Vec<u8>>> {
        let _fetch = self.0.phase(JobPhase::SubstituteFetch);
        let (_, body) =
            download_one_presigned(crate::http::download_client(), upstream.clone(), progress)
                .await?;
        Ok(body.map(|(bytes, _)| bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::progress::Recorded;
    use std::collections::BTreeMap;

    struct Fake {
        have: HashSet<String>,
        upstream: BTreeMap<String, CachedPath>,
        bodies: BTreeMap<String, Vec<u8>>,
        downloads: Vec<String>,
    }

    impl UpstreamIo for Fake {
        async fn have(&mut self, paths: Vec<String>) -> Result<HashSet<String>> {
            Ok(paths
                .into_iter()
                .filter(|p| self.have.contains(p))
                .collect())
        }

        async fn locate(&mut self, path: &str) -> Result<Option<CachedPath>> {
            Ok(self.upstream.get(path).cloned())
        }

        async fn download(
            &mut self,
            upstream: &CachedPath,
            progress: &mut Progress<impl ProgressSink>,
        ) -> Result<Option<Vec<u8>>> {
            self.downloads.push(upstream.path.clone());
            let body = self.bodies.get(&upstream.path).cloned();
            if let Some(body) = &body {
                progress.at(body.len() as u64);
            }
            Ok(body)
        }
    }

    fn nar() -> Vec<u8> {
        b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1\x00\x00\x00".to_vec()
    }

    fn upstream(path: &str, nar_hash: &str, references: &[&str]) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: true,
            file_size: None,
            nar_size: Some(nar().len() as u64),
            url: Some(format!("https://cache.example/{path}.nar.zst")),
            multipart: None,
            nar_hash: Some(nar_hash.to_owned()),
            file_hash: None,
            references: Some(references.iter().map(|r| (*r).to_owned()).collect()),
            signatures: None,
            deriver: Some("aaaa-x.drv".to_owned()),
            ca: None,
        }
    }

    fn fake(path: &str, nar_hash: &str, references: &[&str]) -> Fake {
        Fake {
            have: HashSet::new(),
            upstream: BTreeMap::from([(path.to_owned(), upstream(path, nar_hash, references))]),
            bodies: BTreeMap::from([(
                path.to_owned(),
                zstd::encode_all(nar().as_slice(), 3).unwrap(),
            )]),
            downloads: Vec::new(),
        }
    }

    const OUT: &str = "/nix/store/oooooooooooooooooooooooooooooooo-out";

    #[tokio::test]
    async fn an_output_already_in_our_cache_is_neither_located_nor_fetched() {
        let mut io = fake(OUT, &sha256_nix32(&nar()), &[]);
        io.have.insert(OUT.to_owned());

        let fetched = fetch_outputs(
            &mut io,
            &[("out".to_owned(), OUT.to_owned())],
            &mut Progress::silent(),
        )
        .await
        .unwrap();

        assert!(fetched[0].nar.is_none());
        assert!(io.downloads.is_empty());
    }

    #[tokio::test]
    async fn progress_counts_only_what_is_fetched_against_the_declared_sizes() {
        const CACHED: &str = "/nix/store/cccccccccccccccccccccccccccccccc-dev";
        let mut io = fake(OUT, &sha256_nix32(&nar()), &[]);
        io.have.insert(CACHED.to_owned());
        let body_len = io.bodies[OUT].len() as u64;
        io.upstream.get_mut(OUT).unwrap().file_size = Some(body_len);
        let mut sent = Recorded::default();

        fetch_outputs(
            &mut io,
            &[
                ("out".to_owned(), OUT.to_owned()),
                ("dev".to_owned(), CACHED.to_owned()),
            ],
            &mut Progress::new(&mut sent),
        )
        .await
        .unwrap();

        assert_eq!(sent.0, vec![(body_len, Some(body_len))]);
    }

    #[test]
    fn one_undeclared_size_leaves_the_total_unknown() {
        let sized = |size| CachedPath {
            file_size: size,
            ..upstream(OUT, "", &[])
        };

        assert_eq!(
            download_size([sized(Some(3)), sized(Some(4))].iter()),
            Some(7)
        );
        assert_eq!(download_size([sized(Some(3)), sized(None)].iter()), None);
    }

    #[tokio::test]
    async fn an_output_no_upstream_serves_is_a_substitute_miss() {
        let mut io = fake(OUT, &sha256_nix32(&nar()), &[]);
        io.upstream.clear();

        let err = fetch_outputs(
            &mut io,
            &[("out".to_owned(), OUT.to_owned())],
            &mut Progress::silent(),
        )
        .await
        .unwrap_err();

        assert!(
            err.downcast_ref::<SubstituteNotOnUpstream>().is_some(),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_vanished_upstream_object_is_a_missing_input() {
        let mut io = fake(OUT, &sha256_nix32(&nar()), &[]);
        io.bodies.clear();

        let err = fetch_outputs(
            &mut io,
            &[("out".to_owned(), OUT.to_owned())],
            &mut Progress::silent(),
        )
        .await
        .unwrap_err();

        assert!(err.downcast_ref::<MissingInputs>().is_some(), "{err}");
    }

    #[tokio::test]
    async fn a_nar_that_does_not_hash_to_its_narinfo_is_corrupt() {
        let mut io = fake(
            OUT,
            "sha256:0000000000000000000000000000000000000000000000000000",
            &[],
        );

        let err = fetch_outputs(
            &mut io,
            &[("out".to_owned(), OUT.to_owned())],
            &mut Progress::silent(),
        )
        .await
        .unwrap_err();

        assert!(err.downcast_ref::<CorruptCachedNar>().is_some(), "{err}");
    }

    /// References travel as `hash-name` base names, the shape the narinfo
    /// `References:` line uses; nothing below the output is fetched.
    #[tokio::test]
    async fn references_travel_as_base_names_and_are_not_fetched() {
        let dep = "/nix/store/dddddddddddddddddddddddddddddddd-dep";
        let mut io = fake(
            OUT,
            &sha256_nix32(&nar()),
            &[dep, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-bare"],
        );

        let fetched = fetch_outputs(
            &mut io,
            &[("out".to_owned(), OUT.to_owned())],
            &mut Progress::silent(),
        )
        .await
        .unwrap();

        let raw = fetched[0].nar.as_ref().unwrap();
        assert_eq!(
            raw.references,
            vec![
                "dddddddddddddddddddddddddddddddd-dep".to_owned(),
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-bare".to_owned()
            ]
        );
        assert_eq!(raw.nar, nar());
        assert_eq!(io.downloads, vec![OUT.to_owned()]);
    }
}
