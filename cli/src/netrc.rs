/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{CliError, FileAction};
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

/// curl and therefore nix are keying netrc lookups by host only, without scheme, port and path.
pub fn machine_host(server: &str) -> String {
    let server = server.trim();
    let with_scheme = if server.contains("://") {
        server.to_string()
    } else {
        format!("https://{server}")
    };
    reqwest::Url::parse(&with_scheme)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.trim_matches(['[', ']']).to_string())
        })
        .unwrap_or_default()
}

/// Gradient is ignoring the login and treating the password as the API token.
pub fn entry(host: &str, token: &str) -> String {
    format!("machine {host}\nlogin gradient\npassword {token}\n")
}

pub fn remove_entry(contents: &str, host: &str) -> String {
    if host.is_empty() {
        return contents.to_string();
    }
    let mut result = String::new();
    let mut skip = false;
    for line in contents.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("machine") => skip = words.next() == Some(host),
            Some("default") => skip = false,
            _ => {}
        }
        if !skip {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

pub fn replace_entry_for_host_atomically(
    path: &Path,
    host: &str,
    token: &str,
) -> Result<(), CliError> {
    let existing = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(CliError::file(FileAction::Read, path, e)),
    };
    let updated = remove_entry(&existing, host) + &entry(host, token);

    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).map_err(|e| CliError::file(FileAction::Create, parent, e))?;
    let write_error = |e| CliError::file(FileAction::Write, path, e);
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(write_error)?;
    staged.write_all(updated.as_bytes()).map_err(write_error)?;
    staged.as_file().sync_all().map_err(write_error)?;
    staged.persist(path).map_err(|e| write_error(e.error))?;
    Ok(())
}

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
        assert_eq!(
            entry("h", "GRADabc"),
            "machine h\nlogin gradient\npassword GRADabc\n"
        );
    }

    #[test]
    fn remove_entry_drops_matching_machine_block() {
        let contents =
            "machine a\nlogin gradient\npassword 1\nmachine b\nlogin gradient\npassword 2\n";
        let out = remove_entry(contents, "a");
        assert!(!out.contains("password 1"), "{out}");
        assert!(out.contains("machine b"), "{out}");
    }

    #[test]
    fn machine_host_keeps_ipv6_addresses() {
        assert_eq!(machine_host("https://[::1]:8080/"), "::1");
    }

    #[test]
    fn remove_entry_keeps_a_default_block_after_the_matching_machine() {
        let contents = "machine a\npassword 1\ndefault\nlogin anonymous\n";
        let out = remove_entry(contents, "a");
        assert_eq!(out, "default\nlogin anonymous\n");
    }

    #[test]
    fn installed_entry_is_readable_by_the_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("netrc");
        fs::write(&path, "machine other\npassword 2\n").unwrap();

        replace_entry_for_host_atomically(&path, "h", "GRADabc").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "machine other\npassword 2\nmachine h\nlogin gradient\npassword GRADabc\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn an_unreadable_netrc_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("netrc");
        fs::write(&path, [0xff, 0xfe]).unwrap();

        assert!(replace_entry_for_host_atomically(&path, "h", "GRADabc").is_err());
        assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe]);
    }
}
