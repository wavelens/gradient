/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::content_addressed_store_path_from_name_address_references::path_for;
use harmonia_store_content_address::ContentAddress;
use harmonia_store_path::{StorePath, StorePathSet};
use harmonia_utils_hash::{Hash, Sha256};

pub struct StorePathWithContentAddress {
    pub store_path: String,
    pub content_address: String,
}

pub fn compute_derivation_store_path_and_text_content_address(
    name: &str,
    text: &[u8],
    references: &[String],
) -> StorePathWithContentAddress {
    let content_address = ContentAddress::Text(Sha256::digest(text));
    let references: StorePathSet = references
        .iter()
        .map(|reference| {
            StorePath::from_base_path(reference.trim_start_matches("/nix/store/")).unwrap()
        })
        .collect();
    StorePathWithContentAddress {
        store_path: format!(
            "/nix/store/{}",
            path_for(name, &content_address, &references).unwrap()
        ),
        content_address: content_address.to_string(),
    }
}

pub fn flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
    name: &str,
    contents: &[u8],
) -> StorePathWithContentAddress {
    let content_address = ContentAddress::Flat(Hash::SHA256(Sha256::digest(contents)));
    StorePathWithContentAddress {
        store_path: format!(
            "/nix/store/{}",
            path_for(name, &content_address, &StorePathSet::new()).unwrap()
        ),
        content_address: content_address.to_string(),
    }
}
