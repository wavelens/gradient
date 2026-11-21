/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use harmonia_store_content_address::{ContentAddress, make_store_path_from_ca};
use harmonia_store_path::{StoreDir, StorePath, StorePathSet};
use harmonia_utils_hash::{Hash, Sha256};

fn fingerprinted(
    kind: &str,
    refs: &StorePathSet,
    digest: &Sha256,
    name: &str,
) -> anyhow::Result<StorePath> {
    let store_dir = StoreDir::default();
    let refs: String = refs
        .iter()
        .map(|r| format!(":{}", store_dir.display(r)))
        .collect();
    let fingerprint = format!("{kind}{refs}:sha256:{digest:x}:{store_dir}:{name}");
    Ok(StorePath::from_hash(
        &Sha256::digest(fingerprint),
        name.parse()?,
    ))
}

pub fn path_for(name: &str, ca: &ContentAddress, refs: &StorePathSet) -> anyhow::Result<StorePath> {
    match ca {
        ContentAddress::Text(digest) => fingerprinted("text", refs, digest, name),
        ContentAddress::NixArchive(Hash::SHA256(digest)) => {
            fingerprinted("source", refs, digest, name)
        }
        other => Ok(make_store_path_from_ca(
            &StoreDir::default(),
            name.parse()?,
            *other,
        )),
    }
}
