/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Helpers for parsing Hydra build-product metadata.

/// Parse a single `hydra-build-products` line.
///
/// Returns `(file_type, file_path)` for lines of the form `file <type> <path>`,
/// `None` for blank lines or lines with a different prefix.
pub fn parse_hydra_product_line(line: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 3 && parts[0] == "file" {
        Some((parts[1].to_string(), parts[2..].join(" ")))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typical() {
        let got = parse_hydra_product_line("file doc /nix/store/xyz/share/doc/index.html");
        assert_eq!(
            got,
            Some((
                "doc".to_string(),
                "/nix/store/xyz/share/doc/index.html".to_string()
            ))
        );
    }

    #[test]
    fn parse_rejoins_paths_with_spaces() {
        let got = parse_hydra_product_line("file report /tmp/my report.txt");
        assert_eq!(
            got,
            Some(("report".to_string(), "/tmp/my report.txt".to_string()))
        );
    }

    #[test]
    fn parse_rejects_non_file_prefix() {
        assert_eq!(parse_hydra_product_line("dir doc /x"), None);
    }

    #[test]
    fn parse_rejects_too_few_parts() {
        assert_eq!(parse_hydra_product_line("file doc"), None);
        assert_eq!(parse_hydra_product_line("file"), None);
        assert_eq!(parse_hydra_product_line(""), None);
    }
}
