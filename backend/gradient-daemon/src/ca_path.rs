/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::content_addressed_store_path_from_name_address_references::path_for;
use harmonia_store_content_address::{ContentAddress, ContentAddressMethodAlgorithm};
use harmonia_store_path::{StorePath, StorePathSet};
use harmonia_utils_hash::Sha256;

pub fn ca_path(
    name: &str,
    method: ContentAddressMethodAlgorithm,
    refs: &StorePathSet,
    dump: &[u8],
) -> anyhow::Result<(StorePath, ContentAddress)> {
    let ca = match method {
        ContentAddressMethodAlgorithm::Text => ContentAddress::Text(Sha256::digest(dump)),
        ContentAddressMethodAlgorithm::NixArchive(algo) => {
            ContentAddress::NixArchive(algo.digest(dump))
        }
        ContentAddressMethodAlgorithm::Flat(algo) => ContentAddress::Flat(algo.digest(dump)),
    };
    Ok((path_for(name, &ca, refs)?, ca))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(base: &str) -> StorePath {
        StorePath::from_base_path(base).expect("store path")
    }

    #[test]
    fn text_without_references_matches_nix() {
        let (path, _) = ca_path(
            "x",
            ContentAddressMethodAlgorithm::Text,
            &StorePathSet::new(),
            b"y",
        )
        .expect("ca");
        assert_eq!(path.to_string(), "lfngsssysp6h1v4ccqg23c52s9sjl779-x");
    }

    #[test]
    fn text_with_references_matches_nix() {
        let refs = StorePathSet::from([sp("lfngsssysp6h1v4ccqg23c52s9sjl779-x")]);
        let content = b"ref /nix/store/lfngsssysp6h1v4ccqg23c52s9sjl779-x";
        let (path, _) =
            ca_path("a", ContentAddressMethodAlgorithm::Text, &refs, content).expect("ca");
        assert_eq!(path.to_string(), "1fhyi97xq1his2v298sj7x8wiibiafc2-a");
    }

    #[test]
    fn path_for_agrees_with_ca_path() {
        let refs = StorePathSet::new();
        let (path, ca) =
            ca_path("x", ContentAddressMethodAlgorithm::Text, &refs, b"y").expect("ca");
        assert_eq!(path_for("x", &ca, &refs).expect("path_for"), path);
    }
}
