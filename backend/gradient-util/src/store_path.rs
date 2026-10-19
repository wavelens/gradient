/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

pub fn strip_store_prefix(path: &str) -> &str {
    path.strip_prefix("/nix/store/").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_store_path_adds_the_store_prefix() {
        for (input, expected) in [
            ("abc123-foo", "/nix/store/abc123-foo"),
            ("/nix/store/abc123-foo", "/nix/store/abc123-foo"),
            ("/tmp/foo", "/tmp/foo"),
            ("", "/nix/store/"),
        ] {
            assert_eq!(nix_store_path(input), expected, "{input}");
        }
    }

    #[test]
    fn strip_store_prefix_borrows_from_the_input() {
        let input = "/nix/store/abc123-foo";
        let out = strip_store_prefix(input);
        assert_eq!(out, "abc123-foo");
        assert!(std::ptr::eq(out.as_ptr(), input[11..].as_ptr()));

        let input = "abc123-foo";
        let out = strip_store_prefix(input);
        assert_eq!(out, "abc123-foo");
        assert!(std::ptr::eq(out.as_ptr(), input.as_ptr()));
    }
}
