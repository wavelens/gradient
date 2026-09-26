/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A Download spec: nix's `builtin:fetchurl`, done by the worker itself. The `.drv`
//! comes from our cache (the evaluation pushed it), the URL is fetched, the fixed
//! output hash is checked, and the result is packed as a NAR for the same push every
//! other kind ends in. No nix store is touched.

use std::fmt;

use anyhow::{Context, Result, bail};
use gradient_util::nix_hash::nix32_encode;
use gradient_wire::messages::{BuildSpec, QueryMode};
use sha2::{Digest, Sha256};

use super::substitute::RawNar;
use crate::proto::compression::{
    decompress, extract_single_file_from_nar, parse_nar_hash_to_bytes, resolve_compression,
};
use crate::proto::job::JobUpdater;
use crate::proto::prefetch::{MissingInputs, download_one_presigned};
use crate::proto::progress::{Progress, ProgressSink, read_body};

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

fn nar_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s);
    out.extend(std::iter::repeat_n(0u8, (8 - s.len() % 8) % 8));
}

pub(crate) fn single_file_nar(contents: &[u8], executable: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 128);
    for token in [b"nix-archive-1".as_slice(), b"(", b"type", b"regular"] {
        nar_str(&mut out, token);
    }
    if executable {
        nar_str(&mut out, b"executable");
        nar_str(&mut out, b"");
    }
    nar_str(&mut out, b"contents");
    nar_str(&mut out, contents);
    nar_str(&mut out, b")");
    out
}

/// `outputHash` in any of nix's spellings: SRI, base16, nix32 or base64.
fn parse_sha256(text: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let body = text
        .strip_prefix("sha256-")
        .or_else(|| text.strip_prefix("sha256:"))
        .unwrap_or(text);
    Ok(match body.len() {
        64 => hex::decode(body).context("base16 outputHash")?,
        52 => parse_nar_hash_to_bytes(&format!("sha256:{body}"))?.to_vec(),
        44 => base64::engine::general_purpose::STANDARD
            .decode(body)
            .context("base64 outputHash")?,
        n => bail!("outputHash of {n} characters is not a sha256"),
    })
}

pub(crate) fn fetch_spec(drv: &gradient_db::Derivation, drv_path: &str) -> Result<FetchSpec> {
    if drv.builder != "builtin:fetchurl" {
        return Err(anyhow::Error::new(UnsupportedFetch(drv.builder.clone())));
    }
    let env = |key: &str| drv.environment.get(key).map(String::as_str).unwrap_or("");
    if env("outputHashAlgo") != "sha256" {
        return Err(anyhow::Error::new(UnsupportedFetch(format!(
            "outputHashAlgo {}",
            env("outputHashAlgo")
        ))));
    }
    let digest = parse_sha256(env("outputHash"))?;
    let hash = match env("outputHashMode") {
        "recursive" => FixedHash::Recursive(digest),
        _ => FixedHash::Flat(digest),
    };
    let output_path = drv
        .outputs
        .iter()
        .find(|o| o.name == "out")
        .map(|o| o.path.clone())
        .context("builtin:fetchurl has an out output")?;

    Ok(FetchSpec {
        url: env("url").to_owned(),
        unpack: env("unpack") == "1",
        executable: env("executable") == "1",
        hash,
        output_path,
        drv_base: drv_path.trim_start_matches("/nix/store/").to_owned(),
    })
}

pub(crate) trait DownloadIo {
    async fn drv(&mut self, drv_path: &str) -> Result<gradient_db::Derivation>;
    async fn get(
        &mut self,
        url: &str,
        progress: &mut Progress<impl ProgressSink>,
    ) -> Result<Option<Vec<u8>>>;
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
    drv: &gradient_db::Derivation,
    task: &BuildSpec,
    progress: &mut Progress<impl ProgressSink>,
) -> Result<(String, RawNar)> {
    let spec = fetch_spec(drv, &task.drv_path)?;
    let body = io
        .get(&spec.url, progress)
        .await?
        .ok_or_else(|| anyhow::Error::new(UnsupportedFetch(format!("{} is gone", spec.url))))?;
    let (nar, flat) = if spec.unpack {
        (
            decompress(&body, resolve_compression(&body, Some(&spec.url)))?,
            None,
        )
    } else {
        (single_file_nar(&body, spec.executable), Some(body))
    };
    let (hashed, recursive, expected) = match &spec.hash {
        FixedHash::Flat(d) => (flat.as_deref().unwrap_or(&nar), false, d),
        FixedHash::Recursive(d) => (nar.as_slice(), true, d),
    };
    let digest = Sha256::digest(hashed);
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
            references: Vec::new(),
            deriver: Some(spec.drv_base),
            ca: Some(ca),
        },
    ))
}

pub(crate) struct JobUpdaterIo<'a>(pub &'a mut JobUpdater);

impl DownloadIo for JobUpdaterIo<'_> {
    async fn drv(&mut self, drv_path: &str) -> Result<gradient_db::Derivation> {
        let entry = self
            .0
            .query_cache(vec![drv_path.to_owned()], QueryMode::Pull)
            .await?
            .into_iter()
            .find(|cp| cp.cached)
            .ok_or_else(|| anyhow::Error::new(MissingInputs(vec![drv_path.to_owned()])))?;
        let compressed = if entry.url.is_some() {
            download_one_presigned(
                crate::http::download_client(),
                entry.clone(),
                &mut Progress::silent(),
            )
            .await?
            .1
            .map(|(bytes, _)| bytes)
        } else {
            // One NAR over the stream, read the way `prefetch::fetch_by_request` reads it.
            match self
                .0
                .request_nars(vec![drv_path.to_owned()])
                .await?
                .into_iter()
                .next()
            {
                Some((_, payload)) => Some(payload.read_bytes().await?.into_owned()),
                None => None,
            }
        }
        .ok_or_else(|| anyhow::Error::new(MissingInputs(vec![drv_path.to_owned()])))?;
        let nar = decompress(
            &compressed,
            resolve_compression(&compressed, entry.url.as_deref()),
        )?;
        gradient_db::parse_drv(&extract_single_file_from_nar(&nar).await?)
    }

    async fn get(
        &mut self,
        url: &str,
        progress: &mut Progress<impl ProgressSink>,
    ) -> Result<Option<Vec<u8>>> {
        let response = crate::http::download_client().get(url).send().await?;
        if matches!(response.status().as_u16(), 404 | 410) {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        let size = response.content_length();
        progress.set_total(size);
        let body = read_body(response, size, progress).await?;
        progress.finish().await;
        Ok(Some(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::messages::BuildSpecKind;
    use std::collections::{BTreeMap, HashMap};

    const OUT: &str = "/nix/store/oooooooooooooooooooooooooooooooo-hello.txt";
    const DRV: &str = "/nix/store/dddddddddddddddddddddddddddddddd-hello.txt.drv";

    fn drv(env: &[(&str, &str)]) -> gradient_db::Derivation {
        gradient_db::Derivation {
            outputs: vec![gradient_db::DerivationOutput {
                name: "out".to_owned(),
                path: OUT.to_owned(),
                hash_algo: String::new(),
                hash: String::new(),
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
        async fn drv(&mut self, drv_path: &str) -> Result<gradient_db::Derivation> {
            gradient_db::parse_drv(self.drvs.get(drv_path).expect("drv in cache"))
        }

        async fn get(
            &mut self,
            url: &str,
            _: &mut Progress<impl ProgressSink>,
        ) -> Result<Option<Vec<u8>>> {
            Ok(self.bodies.get(url).cloned())
        }
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

    /// The NAR of one regular file, as `nix-store --dump` writes it: every token
    /// length-prefixed and padded to eight bytes, and the parser reads it back.
    #[tokio::test]
    async fn a_flat_download_is_packed_as_one_regular_file() {
        let nar = single_file_nar(b"hi\n", false);
        assert_eq!(nar.len(), 120);
        assert!(nar.starts_with(
            b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00(\x00\x00\x00\x00\x00\x00\x00"
        ));
        let back = crate::proto::compression::extract_single_file_from_nar(&nar)
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
            &drv(&[
                ("url", "https://example.org/hello.txt"),
                ("outputHash", &sha256_hex(b"hi\n")),
                ("outputHashAlgo", "sha256"),
                ("outputHashMode", "flat"),
                ("executable", "1"),
            ]),
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
    fn a_builder_that_is_not_fetchurl_is_unsupported() {
        let mut d = drv(&[("url", "https://example.org/x")]);
        d.builder = "/nix/store/bbbb-bash/bin/bash".to_owned();
        let err = fetch_spec(&d, DRV).unwrap_err();
        assert!(err.downcast_ref::<UnsupportedFetch>().is_some(), "{err}");
    }

    #[tokio::test]
    async fn a_flat_download_is_verified_then_packed() {
        let d = drv(&[
            ("url", "https://example.org/hello.txt"),
            ("outputHash", &sha256_hex(b"hi\n")),
            ("outputHashAlgo", "sha256"),
            ("outputHashMode", "flat"),
        ]);
        let mut io = body("https://example.org/hello.txt", b"hi\n");

        let (path, raw) = download_with(&mut io, &d, &task(), &mut Progress::silent())
            .await
            .unwrap();

        assert_eq!(path, OUT);
        assert_eq!(raw.nar, single_file_nar(b"hi\n", false));
        assert_eq!(
            raw.deriver.as_deref(),
            Some("dddddddddddddddddddddddddddddddd-hello.txt.drv")
        );
        assert!(raw.ca.as_deref().unwrap().starts_with("fixed:sha256:"));
        assert!(raw.references.is_empty());
    }

    #[tokio::test]
    async fn a_hash_mismatch_is_a_fixed_output_mismatch() {
        let d = drv(&[
            ("url", "https://example.org/hello.txt"),
            ("outputHash", &sha256_hex(b"not this")),
            ("outputHashAlgo", "sha256"),
            ("outputHashMode", "flat"),
        ]);
        let mut io = body("https://example.org/hello.txt", b"hi\n");

        let err = download_with(&mut io, &d, &task(), &mut Progress::silent())
            .await
            .unwrap_err();

        assert!(err.downcast_ref::<FixedOutputMismatch>().is_some(), "{err}");
    }

    /// `unpack = 1`: the download IS the output's NAR (possibly compressed) and the
    /// hash is recursive, over the NAR.
    #[tokio::test]
    async fn an_unpacked_download_is_the_nar_itself() {
        let nar = single_file_nar(b"hi\n", false);
        let d = drv(&[
            ("url", "https://example.org/hello.nar.xz"),
            ("outputHash", &sha256_hex(&nar)),
            ("outputHashAlgo", "sha256"),
            ("outputHashMode", "recursive"),
            ("unpack", "1"),
        ]);
        let mut io = body("https://example.org/hello.nar.xz", &xz_encode(&nar));

        let (_, raw) = download_with(&mut io, &d, &task(), &mut Progress::silent())
            .await
            .unwrap();

        assert_eq!(raw.nar, nar);
        assert!(raw.ca.as_deref().unwrap().starts_with("fixed:r:sha256:"));
    }
}
