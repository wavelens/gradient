/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Builds the Nix narinfo fingerprint that caches sign and verifiers check:
/// `1;{store_path};{nar_hash};{nar_size};{refs}` where `refs` are
/// `/nix/store/`-prefixed, sorted, and comma-joined. Shared by the signing and
/// verification paths so both normalize references identically.
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
