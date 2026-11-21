/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::NixPathInfo;

pub(super) fn narinfo_signing_fingerprint(path_info: &NixPathInfo) -> String {
    fingerprint(
        &path_info.store_path,
        &path_info.nar_hash,
        path_info.nar_size,
        path_info.references.iter().map(String::as_str),
    )
}

pub(super) fn fingerprint<'a, I, N>(
    store_path: &str,
    nar_hash: &str,
    nar_size: N,
    references: I,
) -> String
where
    I: IntoIterator<Item = &'a str>,
    N: std::fmt::Display,
{
    let mut full_refs: Vec<String> = references
        .into_iter()
        .map(|r| {
            if r.starts_with("/nix/store/") {
                r.to_owned()
            } else {
                format!("/nix/store/{}", r)
            }
        })
        .collect();
    full_refs.sort();
    format!(
        "1;{};{};{};{}",
        store_path,
        nar_hash,
        nar_size,
        full_refs.join(",")
    )
}
