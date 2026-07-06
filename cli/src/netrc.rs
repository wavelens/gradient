/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Netrc helpers shared by `cache install-netrc` (a persistent, merged netrc
//! file) and the build path's private-cache output substitution (an ephemeral
//! 0600 temp file for a single nix invocation).

/// The netrc `machine` for a server URL: host only, with scheme, port and path
/// stripped, matching how curl - and therefore nix - keys netrc lookups.
pub fn machine_host(server: &str) -> String {
    server
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', ':'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// A netrc entry authorising `host` with `token`. Gradient ignores the login and
/// treats the password as the API token.
pub fn entry(host: &str, token: &str) -> String {
    format!("machine {host}\nlogin gradient\npassword {token}\n")
}

/// `contents` with any existing entry for `host` removed, so a re-install
/// replaces rather than duplicates it.
pub fn remove_entry(contents: &str, host: &str) -> String {
    if host.is_empty() {
        return contents.to_string();
    }
    let mut result = String::new();
    let mut skip = false;
    for line in contents.lines() {
        if line.starts_with("machine ") {
            skip = line.split_whitespace().nth(1) == Some(host);
        }
        if !skip {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// Write a single-entry netrc to a fresh 0600 temp file for one nix invocation.
#[cfg(feature = "nix")]
pub fn temp_file(host: &str, token: &str) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write as _;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(entry(host, token).as_bytes())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_host_strips_scheme_port_and_path() {
        assert_eq!(
            machine_host("https://gradient.example.com/cache/x"),
            "gradient.example.com"
        );
        assert_eq!(machine_host("http://host:8080/"), "host");
        assert_eq!(machine_host("gradient.example.com"), "gradient.example.com");
    }

    #[test]
    fn entry_uses_token_as_password() {
        assert_eq!(entry("h", "GRADabc"), "machine h\nlogin gradient\npassword GRADabc\n");
    }

    #[test]
    fn remove_entry_drops_matching_machine_block() {
        let contents =
            "machine a\nlogin gradient\npassword 1\nmachine b\nlogin gradient\npassword 2\n";
        let out = remove_entry(contents, "a");
        assert!(!out.contains("password 1"), "{out}");
        assert!(out.contains("machine b"), "{out}");
    }
}
