/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Stable per-report pseudonyms for the strings a report would otherwise leak.

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest as _, Sha256};

use crate::schema::ReportOptions;

/// Maps identifying strings to tokens. The salt is random per report and never
/// written to the file, so tokens correlate within one report and not across
/// two of the same instance.
pub struct Redactor {
    opts: ReportOptions,
    salt: [u8; 32],
    seen: Mutex<HashMap<String, String>>,
}

impl Redactor {
    pub fn new(opts: ReportOptions) -> Self {
        Self {
            opts,
            salt: rand::random(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    fn token(&self, value: &str, kind: &str) -> String {
        let key = format!("{kind}\u{1f}{value}");
        let mut guard = self.seen.lock().expect("redactor mutex");
        if let Some(existing) = guard.get(&key) {
            return existing.clone();
        }

        let mut hasher = Sha256::new();
        hasher.update(self.salt);
        hasher.update(key.as_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        let token = format!(
            "{kind}-{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3]
        );
        guard.insert(key, token.clone());
        token
    }

    pub fn identity(&self, value: &str, kind: &str) -> String {
        if !self.opts.anonymize_identities || value.is_empty() {
            return value.to_owned();
        }

        self.token(value, kind)
    }

    pub fn package(&self, value: &str) -> String {
        if !self.opts.anonymize_packages || value.is_empty() {
            return value.to_owned();
        }

        self.token(value, "pkg")
    }

    /// Rewrites only the name half of a store path. The 32-char hash stays: it
    /// is one-way, and it is what makes an upstream cache check possible.
    pub fn store_path(&self, path: &str) -> String {
        if !self.opts.anonymize_packages || path.is_empty() {
            return path.to_owned();
        }

        let Some(rest) = path.strip_prefix("/nix/store/") else {
            return self.package(path);
        };

        match rest.split_once('-') {
            Some((hash, name)) => format!("/nix/store/{hash}-{}", self.package(name)),
            None => path.to_owned(),
        }
    }

    /// Space-separated store paths, as `derivation_output.references_list`
    /// stores them.
    pub fn store_path_list(&self, list: &str) -> String {
        if !self.opts.anonymize_packages || list.is_empty() {
            return list.to_owned();
        }

        list.split_whitespace()
            .map(|p| self.store_path(p))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(identities: bool, packages: bool) -> ReportOptions {
        ReportOptions {
            anonymize_identities: identities,
            anonymize_packages: packages,
            include_logs: true,
            include_instance: true,
        }
    }

    const REPO: &str = "git@git.supersandro.de:sandro/nixos-config.git";

    #[test]
    fn a_pseudonym_is_stable_within_a_report_and_never_the_original() {
        let r = Redactor::new(opts(true, false));
        let first = r.identity(REPO, "repo");
        assert_eq!(first, r.identity(REPO, "repo"), "same input, same token");
        assert_ne!(first, REPO);
        assert!(
            !first.contains("supersandro"),
            "token leaked the original: {first}"
        );
        assert!(
            first.starts_with("repo-"),
            "token should name its kind: {first}"
        );
    }

    #[test]
    fn two_reports_of_the_same_instance_are_uncorrelatable() {
        let a = Redactor::new(opts(true, false)).identity(REPO, "repo");
        let b = Redactor::new(opts(true, false)).identity(REPO, "repo");
        assert_ne!(a, b, "a fresh salt must produce a fresh token");
    }

    #[test]
    fn the_toggles_are_independent() {
        let r = Redactor::new(opts(true, false));
        assert_eq!(r.package("hello-2.12"), "hello-2.12", "packages stay real");
        assert_ne!(r.identity(REPO, "repo"), REPO, "identities do not");

        let r = Redactor::new(opts(false, true));
        assert_eq!(r.identity(REPO, "repo"), REPO);
        assert_ne!(r.package("hello-2.12"), "hello-2.12");
    }

    /// The hash half is one-way and is what lets a maintainer check a path
    /// against a public cache, so it survives even full anonymisation.
    #[test]
    fn store_path_keeps_its_hash_and_renames_only_the_package() {
        let r = Redactor::new(opts(true, true));
        let out = r.store_path("/nix/store/2s7ijz3qblblfb903r4spy3pvd7ag35f-clap_complete-4.6.9");
        assert!(
            out.starts_with("/nix/store/2s7ijz3qblblfb903r4spy3pvd7ag35f-"),
            "{out}"
        );
        assert!(!out.contains("clap_complete"), "{out}");
    }

    #[test]
    fn store_path_is_untouched_when_packages_are_not_anonymized() {
        let p = "/nix/store/2s7ijz3qblblfb903r4spy3pvd7ag35f-clap_complete-4.6.9";
        assert_eq!(Redactor::new(opts(true, false)).store_path(p), p);
    }

    #[test]
    fn a_reference_list_is_redacted_entry_by_entry() {
        let r = Redactor::new(opts(true, true));
        let out = r.store_path_list(
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-glibc-2.40 \
             /nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-openssl-3.5",
        );
        assert_eq!(out.split_whitespace().count(), 2);
        assert!(!out.contains("glibc") && !out.contains("openssl"), "{out}");
        assert!(out.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "{out}");
    }
}
