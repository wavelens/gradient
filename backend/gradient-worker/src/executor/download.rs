/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The worker is doing nix's `builtin:fetchurl` itself without touching a nix store.
//! The `.drv` is coming from our cache because the evaluation pushed it.

use std::fmt;

use anyhow::{Context, Result, bail};
use gradient_derivation::DrvOutputSpec;
use gradient_util::nix_hash::nix32_encode;
use gradient_wire::messages::{BuildSpec, QueryMode};
use std::sync::Arc;

use super::substitute::RawNar;
use crate::proto::job::JobUpdater;
use crate::proto::nar_daemon_import::{FetchedNar, FlatFileNar, digest_of};
use crate::proto::prefetch::{MissingInputs, download_one_presigned};
use crate::proto::progress::{Progress, ProgressSink, Tally, stage_body};
use gradient_worker_client::compression::{
    decompress, extract_single_file_from_nar, resolve_compression,
};
use gradient_worker_client::nar::NarReader;
use gradient_worker_client::nar_recv::NarPayload;

#[derive(Debug)]
pub(crate) struct FixedOutputMismatch(pub String);
impl fmt::Display for FixedOutputMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fixed output hash mismatch for {}", self.0)
    }
}
impl std::error::Error for FixedOutputMismatch {}

#[derive(Debug)]
pub(crate) struct UnsupportedFetch(pub String);
impl fmt::Display for UnsupportedFetch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a download a worker can perform: {}", self.0)
    }
}
impl std::error::Error for UnsupportedFetch {}

#[derive(Debug)]
pub(crate) enum FixedHash {
    Flat(Vec<u8>),
    Recursive(Vec<u8>),
}

#[derive(Debug)]
pub(crate) struct FetchSpec {
    pub url: String,
    pub unpack: bool,
    pub executable: bool,
    pub hash: FixedHash,
    pub output_path: String,
    pub drv_base: String,
}

pub(crate) fn fetch_spec(
    drv: &gradient_derivation::Derivation,
    drv_path: &str,
) -> Result<FetchSpec> {
    if drv.builder != "builtin:fetchurl" {
        return Err(anyhow::Error::new(UnsupportedFetch(drv.builder.clone())));
    }
    let out = drv
        .outputs
        .iter()
        .find(|o| o.name == "out")
        .context("builtin:fetchurl has an out output")?;
    let DrvOutputSpec::FixedOutput { hash_algo, hash } = out.as_spec() else {
        bail!("builtin:fetchurl output {} has no fixed hash", out.path);
    };
    let digest = hex::decode(hash).context("fixed output hash")?;
    let hash = match hash_algo {
        "sha256" => FixedHash::Flat(digest),
        "r:sha256" => FixedHash::Recursive(digest),
        other => {
            return Err(anyhow::Error::new(UnsupportedFetch(format!(
                "hash algorithm {other}"
            ))));
        }
    };
    let env = |key: &str| drv.environment.get(key).map(String::as_str).unwrap_or("");

    Ok(FetchSpec {
        url: env("url").to_owned(),
        unpack: env("unpack") == "1",
        executable: env("executable") == "1",
        hash,
        output_path: out.path.clone(),
        drv_base: drv_path.trim_start_matches("/nix/store/").to_owned(),
    })
}

pub(crate) trait DownloadIo {
    async fn drv(&mut self, drv_path: &str) -> Result<gradient_derivation::Derivation>;
    async fn get(
        &mut self,
        url: &str,
        output_path: &str,
        progress: &mut Progress<impl ProgressSink>,
    ) -> Result<Option<NarPayload>>;
}

pub(crate) async fn download_output(
    io: &mut impl DownloadIo,
    task: &BuildSpec,
    progress: &mut Progress<impl ProgressSink>,
) -> Result<(String, RawNar)> {
    let drv = io.drv(&task.drv_path).await?;
    download_with(io, &drv, task, progress).await
}

async fn download_with(
    io: &mut impl DownloadIo,
    drv: &gradient_derivation::Derivation,
    task: &BuildSpec,
    progress: &mut Progress<impl ProgressSink>,
) -> Result<(String, RawNar)> {
    let spec = fetch_spec(drv, &task.drv_path)?;
    let body = io
        .get(&spec.url, &spec.output_path, progress)
        .await?
        .ok_or_else(|| anyhow::Error::new(UnsupportedFetch(format!("{} is gone", spec.url))))?;
    let (nar, flat): (Arc<dyn NarReader>, Option<Arc<FlatFileNar>>) = if spec.unpack {
        (
            Arc::new(FetchedNar::new(body, Some(spec.url.clone()))),
            None,
        )
    } else {
        let flat = Arc::new(FlatFileNar::new(body, spec.executable).await);
        (flat.clone(), Some(flat))
    };
    let packed = digest_of(Arc::clone(&nar)).await?;
    let (digest, recursive, expected) = match (&spec.hash, &flat) {
        (FixedHash::Flat(d), Some(flat)) => (flat.file_digest().await?.sha256, false, d),
        (FixedHash::Flat(d), None) => (packed.sha256, false, d),
        (FixedHash::Recursive(d), _) => (packed.sha256, true, d),
    };
    if digest.as_slice() != expected.as_slice() {
        return Err(anyhow::Error::new(FixedOutputMismatch(
            spec.output_path.clone(),
        )));
    }
    let ca = format!(
        "fixed:{}sha256:{}",
        if recursive { "r:" } else { "" },
        nix32_encode(&digest)
    );

    Ok((
        spec.output_path,
        RawNar {
            nar,
            nar_size: packed.size,
            nar_hash: packed.nix32(),
            references: Vec::new(),
            deriver: Some(spec.drv_base),
            ca: Some(ca),
        },
    ))
}

pub(crate) struct JobUpdaterIo<'a>(pub &'a mut JobUpdater);

impl DownloadIo for JobUpdaterIo<'_> {
    async fn drv(&mut self, drv_path: &str) -> Result<gradient_derivation::Derivation> {
        let entry = self
            .0
            .query_cache(vec![drv_path.to_owned()], QueryMode::Pull)
            .await?
            .into_iter()
            .find(|cp| cp.cached)
            .ok_or_else(|| anyhow::Error::new(MissingInputs(vec![drv_path.to_owned()])))?;
        let compressed = if entry.url.is_some() {
            download_one_presigned(
                gradient_worker_client::http::download_client(),
                entry.clone(),
                &mut Progress::silent(),
            )
            .await?
            .1
            .map(|(bytes, _)| bytes)
        } else {
            match self
                .0
                .request_nars(vec![drv_path.to_owned()], &Tally::default())
                .await?
                .into_iter()
                .next()
            {
                Some((_, payload)) => Some(payload.into_bytes().await?),
                None => None,
            }
        }
        .ok_or_else(|| anyhow::Error::new(MissingInputs(vec![drv_path.to_owned()])))?;
        let nar = decompress(
            &compressed,
            resolve_compression(&compressed, entry.url.as_deref()),
        )?;
        gradient_derivation::parse_drv(&extract_single_file_from_nar(&nar).await?)
    }

    async fn get(
        &mut self,
        url: &str,
        output_path: &str,
        progress: &mut Progress<impl ProgressSink>,
    ) -> Result<Option<NarPayload>> {
        let response = gradient_worker_client::http::download_client()
            .get(url)
            .send()
            .await?;
        if matches!(response.status().as_u16(), 404 | 410) {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        let size = response.content_length();
        progress.set_total(size, 1);
        let mut sink = self
            .0
            .nar_recv
            .stage_download(&self.0.job_id, output_path)
            .await?;
        stage_body(response, &mut sink, progress).await?;
        progress.transfer_done();
        progress.finish().await;
        Ok(Some(sink.finish().await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_util::nar::single_file_nar;
    use gradient_wire::messages::BuildSpecKind;
    use std::collections::{BTreeMap, HashMap};

    const OUT: &str = "/nix/store/oooooooooooooooooooooooooooooooo-hello.txt";
    const DRV: &str = "/nix/store/dddddddddddddddddddddddddddddddd-hello.txt.drv";

    fn drv(hash_algo: &str, hash: &str, env: &[(&str, &str)]) -> gradient_derivation::Derivation {
        gradient_derivation::Derivation {
            outputs: vec![gradient_derivation::DerivationOutput {
                name: "out".to_owned(),
                path: OUT.to_owned(),
                hash_algo: hash_algo.to_owned(),
                hash: hash.to_owned(),
            }],
            input_derivations: vec![],
            input_sources: vec![],
            system: "builtin".to_owned(),
            builder: "builtin:fetchurl".to_owned(),
            args: vec![],
            environment: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(bytes))
    }

    fn task() -> BuildSpec {
        BuildSpec {
            build_id: "b".into(),
            drv_path: DRV.into(),
            kind: BuildSpecKind::Download,
            is_fixed_output: true,
            outputs: vec![],
            timeout_secs: None,
            max_silent_secs: None,
        }
    }

    struct Fake {
        drvs: BTreeMap<String, Vec<u8>>,
        bodies: HashMap<String, Vec<u8>>,
    }

    impl DownloadIo for Fake {
        async fn drv(&mut self, drv_path: &str) -> Result<gradient_derivation::Derivation> {
            gradient_derivation::parse_drv(self.drvs.get(drv_path).expect("drv in cache"))
        }

        async fn get(
            &mut self,
            url: &str,
            _: &str,
            _: &mut Progress<impl ProgressSink>,
        ) -> Result<Option<NarPayload>> {
            Ok(self.bodies.get(url).cloned().map(NarPayload::from))
        }
    }

    fn streamed(raw: &RawNar) -> Vec<u8> {
        let mut nar = Vec::new();
        std::io::Read::read_to_end(&mut raw.nar.open().unwrap(), &mut nar).unwrap();
        nar
    }

    fn body(url: &str, bytes: &[u8]) -> Fake {
        Fake {
            drvs: BTreeMap::new(),
            bodies: HashMap::from([(url.to_owned(), bytes.to_vec())]),
        }
    }

    fn xz_encode(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    #[tokio::test]
    async fn a_flat_download_is_packed_as_one_regular_file() {
        let nar = single_file_nar(b"hi\n", false);
        assert_eq!(nar.len(), 120);
        assert!(nar.starts_with(
            b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00(\x00\x00\x00\x00\x00\x00\x00"
        ));
        let back = gradient_worker_client::compression::extract_single_file_from_nar(&nar)
            .await
            .unwrap();
        assert_eq!(back, b"hi\n");
        assert_eq!(
            single_file_nar(b"hi\n", true).len(),
            120 + 24 + 8,
            "the executable marker is two more tokens: `executable` and an empty one"
        );
    }

    #[test]
    fn fetch_spec_reads_the_builtin_fetchurl_environment() {
        let spec = fetch_spec(
            &drv(
                "sha256",
                &sha256_hex(b"hi\n"),
                &[
                    ("url", "https://example.org/hello.txt"),
                    ("executable", "1"),
                ],
            ),
            DRV,
        )
        .unwrap();
        assert_eq!(spec.url, "https://example.org/hello.txt");
        assert!(spec.executable && !spec.unpack);
        assert_eq!(spec.output_path, OUT);
        assert_eq!(
            spec.drv_base,
            "dddddddddddddddddddddddddddddddd-hello.txt.drv"
        );
        assert!(matches!(spec.hash, FixedHash::Flat(_)));
    }

    #[test]
    fn an_sri_hash_with_an_empty_algorithm_is_read_from_the_output() {
        let digest = sha256_hex(b"");
        let spec = fetch_spec(
            &drv(
                "sha256",
                &digest,
                &[
                    ("url", "https://example.org/a"),
                    (
                        "outputHash",
                        "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
                    ),
                    ("outputHashAlgo", ""),
                    ("outputHashMode", "flat"),
                ],
            ),
            DRV,
        )
        .unwrap();
        assert!(matches!(spec.hash, FixedHash::Flat(d) if hex::encode(&d) == digest));
    }

    #[test]
    fn a_hash_algorithm_other_than_sha256_is_unsupported() {
        let d = drv(
            "sha512",
            &"0".repeat(128),
            &[("url", "https://example.org/x")],
        );
        let err = fetch_spec(&d, DRV).unwrap_err();
        assert!(err.downcast_ref::<UnsupportedFetch>().is_some(), "{err}");
    }

    #[test]
    fn a_builder_that_is_not_fetchurl_is_unsupported() {
        let mut d = drv(
            "sha256",
            &sha256_hex(b""),
            &[("url", "https://example.org/x")],
        );
        d.builder = "/nix/store/bbbb-bash/bin/bash".to_owned();
        let err = fetch_spec(&d, DRV).unwrap_err();
        assert!(err.downcast_ref::<UnsupportedFetch>().is_some(), "{err}");
    }

    #[tokio::test]
    async fn a_flat_download_is_verified_then_packed() {
        let d = drv(
            "sha256",
            &sha256_hex(b"hi\n"),
            &[("url", "https://example.org/hello.txt")],
        );
        let mut io = body("https://example.org/hello.txt", b"hi\n");

        let (path, raw) = download_with(&mut io, &d, &task(), &mut Progress::silent())
            .await
            .unwrap();

        assert_eq!(path, OUT);
        assert_eq!(streamed(&raw), single_file_nar(b"hi\n", false));
        assert_eq!(raw.nar_size, single_file_nar(b"hi\n", false).len() as u64);
        assert_eq!(
            raw.deriver.as_deref(),
            Some("dddddddddddddddddddddddddddddddd-hello.txt.drv")
        );
        assert!(raw.ca.as_deref().unwrap().starts_with("fixed:sha256:"));
        assert!(raw.references.is_empty());
    }

    #[tokio::test]
    async fn a_hash_mismatch_is_a_fixed_output_mismatch() {
        let d = drv(
            "sha256",
            &sha256_hex(b"not this"),
            &[("url", "https://example.org/hello.txt")],
        );
        let mut io = body("https://example.org/hello.txt", b"hi\n");

        let err = download_with(&mut io, &d, &task(), &mut Progress::silent())
            .await
            .unwrap_err();

        assert!(err.downcast_ref::<FixedOutputMismatch>().is_some(), "{err}");
    }

    #[tokio::test]
    async fn an_unpacked_download_is_the_nar_itself() {
        let nar = single_file_nar(b"hi\n", false);
        let d = drv(
            "r:sha256",
            &sha256_hex(&nar),
            &[("url", "https://example.org/hello.nar.xz"), ("unpack", "1")],
        );
        let mut io = body("https://example.org/hello.nar.xz", &xz_encode(&nar));

        let (_, raw) = download_with(&mut io, &d, &task(), &mut Progress::silent())
            .await
            .unwrap();

        assert_eq!(streamed(&raw), nar);
        assert!(raw.ca.as_deref().unwrap().starts_with("fixed:r:sha256:"));
    }
}
