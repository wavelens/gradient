/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Helpers for parsing Hydra build-product metadata.
//!
//! `nix-support/hydra-build-products` lines have the format
//! `<type> <subtype> <path> [<defaultpath>]`. Common shapes:
//!   nix-build out /nix/store/xxx
//!   doc readme /nix/store/xxx/README.md
//!   report coverage /nix/store/xxx/coverage.html
//!   file binary-dist /nix/store/xxx/foo.tar.gz

/// Parse a single `hydra-build-products` line into `(type, subtype, path)`.
///
/// Returns `None` for blank lines or any line with fewer than three
/// whitespace-separated tokens. The optional `<defaultpath>` fourth field is
/// ignored.
pub fn parse_hydra_product_line(line: &str) -> Option<(String, String, String)> {
    let mut parts = line.split_whitespace();
    let file_type = parts.next()?.to_string();
    let subtype = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    Some((file_type, subtype, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_type() {
        assert_eq!(
            parse_hydra_product_line("file binary-dist /nix/store/xyz/foo.tar.gz"),
            Some((
                "file".into(),
                "binary-dist".into(),
                "/nix/store/xyz/foo.tar.gz".into()
            )),
        );
    }

    #[test]
    fn parse_doc_type() {
        assert_eq!(
            parse_hydra_product_line("doc readme /nix/store/xyz/README.md"),
            Some((
                "doc".into(),
                "readme".into(),
                "/nix/store/xyz/README.md".into()
            )),
        );
    }

    #[test]
    fn parse_nix_build_type() {
        assert_eq!(
            parse_hydra_product_line("nix-build out /nix/store/xyz"),
            Some(("nix-build".into(), "out".into(), "/nix/store/xyz".into())),
        );
    }

    #[test]
    fn parse_report_with_defaultpath_drops_extra() {
        assert_eq!(
            parse_hydra_product_line("report coverage /nix/store/xyz/cov index.html"),
            Some((
                "report".into(),
                "coverage".into(),
                "/nix/store/xyz/cov".into()
            )),
        );
    }

    #[test]
    fn parse_rejects_too_few_tokens() {
        assert_eq!(parse_hydra_product_line("file binary-dist"), None);
        assert_eq!(parse_hydra_product_line("file"), None);
        assert_eq!(parse_hydra_product_line(""), None);
    }
}
