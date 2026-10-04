/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use gradient_util::nix_hash::{nix32_encode, normalize_nar_hash};
use gradient_wire::types::CachedPath;
use sha2::{Digest as _, Sha256};

use super::store::LocalNixStore;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Adoption {
    Reveal,
    Download,
}

pub(crate) fn adoption(local: Option<&str>, server: Option<&str>) -> Adoption {
    match (local, server) {
        (Some(local), Some(server)) if normalize_nar_hash(local) == normalize_nar_hash(server) => {
            Adoption::Reveal
        }
        _ => Adoption::Download,
    }
}

async fn local_nar_hash(store_path: &str) -> Result<String> {
    let mut stream = harmonia_file_nar::NarByteStream::new(store_path.to_owned().into());
    let mut hasher = Sha256::new();
    while let Some(chunk) = stream.next().await {
        hasher.update(&chunk.context("NAR stream error")?);
    }
    Ok(format!("sha256:{}", nix32_encode(&hasher.finalize())))
}

pub(crate) async fn adopt_hidden(
    store: &LocalNixStore,
    entries: Vec<CachedPath>,
) -> Result<(Vec<CachedPath>, Vec<CachedPath>)> {
    let mut adopted = Vec::new();
    let mut download = Vec::new();
    for entry in entries {
        let local = match store.is_hidden(&entry.path).await? {
            true => Some(local_nar_hash(&entry.path).await?),
            false => None,
        };
        match adoption(local.as_deref(), entry.nar_hash.as_deref()) {
            Adoption::Reveal => adopted.push(entry),
            Adoption::Download => download.push(entry),
        }
    }
    let paths: Vec<String> = adopted.iter().map(|e| e.path.clone()).collect();
    store.reveal(&paths).await?;
    Ok((adopted, download))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "sha256:1b8m03r63zqhnjf7l5wnldhh7c134ap5vpj0850ymkq1iyzicy5s";

    #[test]
    fn a_hidden_path_matching_the_server_hash_is_revealed() {
        assert_eq!(adoption(Some(HASH), Some(HASH)), Adoption::Reveal);
    }

    #[test]
    fn a_hidden_path_with_different_content_is_downloaded() {
        let other = "sha256:0000000000000000000000000000000000000000000000000000";
        assert_eq!(adoption(Some(other), Some(HASH)), Adoption::Download);
    }

    #[test]
    fn a_hidden_path_without_a_server_hash_is_downloaded() {
        assert_eq!(adoption(Some(HASH), None), Adoption::Download);
    }

    #[test]
    fn a_path_missing_on_disk_is_downloaded() {
        assert_eq!(adoption(None, Some(HASH)), Adoption::Download);
    }

    #[test]
    fn the_server_hash_matches_in_sri_form_too() {
        let sri = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
        let nix32 = gradient_util::nix_hash::normalize_nar_hash(sri);
        assert_eq!(adoption(Some(&nix32), Some(sri)), Adoption::Reveal);
    }
}
