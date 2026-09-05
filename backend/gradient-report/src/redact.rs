/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Stable per-report pseudonyms for the strings a report would otherwise leak.

use std::collections::HashMap;
use std::sync::Mutex;

use aho_corasick::{AhoCorasick, MatchKind};
use sha2::{Digest as _, Sha256};

use crate::schema::ReportOptions;

const STORE_PREFIX: &str = "/nix/store/";

fn is_store_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '?' | '=')
}

/// Maps identifying strings to tokens. The salt is random per report and never
/// written to the file, so tokens correlate within one report and not across
/// two of the same instance.
pub struct Redactor {
    opts: ReportOptions,
    salt: [u8; 32],
    seen: Mutex<HashMap<String, String>>,
    /// Free text is rewritten against every pseudonym minted so far. Compiled
    /// once per pattern set and reused: a scan of the text per pseudonym costs
    /// the product of both, which is what made a large report take minutes.
    matcher: Mutex<Option<Matcher>>,
    #[cfg(test)]
    compilations: std::sync::atomic::AtomicUsize,
}

/// The minted pseudonyms, compiled into one pass over the text.
struct Matcher {
    /// How many pseudonyms this was built from; a mint invalidates it.
    covers: usize,
    /// `None` when the pattern set could not be compiled. The fallback then
    /// scans once per pseudonym rather than letting an original through.
    automaton: Option<AhoCorasick>,
    patterns: Vec<String>,
    tokens: Vec<String>,
}

impl Matcher {
    fn build(pairs: Vec<(String, String)>) -> Self {
        let covers = pairs.len();
        let (patterns, tokens): (Vec<String>, Vec<String>) = pairs.into_iter().unzip();
        let automaton = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)
            .inspect_err(|e| {
                tracing::warn!(
                    error = %e,
                    patterns = covers,
                    "report: pseudonym matcher did not compile, redacting the slow way",
                );
            })
            .ok();

        Self {
            covers,
            automaton,
            patterns,
            tokens,
        }
    }

    fn apply(&self, text: &str) -> String {
        match &self.automaton {
            Some(automaton) => automaton.replace_all(text, &self.tokens),
            None => self
                .patterns
                .iter()
                .zip(&self.tokens)
                .fold(text.to_owned(), |acc, (original, token)| {
                    acc.replace(original, token)
                }),
        }
    }
}

impl Redactor {
    pub fn new(opts: ReportOptions) -> Self {
        Self {
            opts,
            salt: rand::random(),
            seen: Mutex::new(HashMap::new()),
            matcher: Mutex::new(None),
            #[cfg(test)]
            compilations: std::sync::atomic::AtomicUsize::new(0),
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

    /// Free text names the same things the columns do, so a log is redacted
    /// against every pseudonym this report has already minted, plus any store
    /// path it happens to name. One pass, longest match first, so a shorter
    /// original that is a prefix of another cannot clip it and a token cannot
    /// be rewritten again by a pseudonym minted later.
    pub fn text(&self, text: &str) -> String {
        // Rewriting store paths mints the package names this text introduces,
        // so the matcher is resolved after it rather than before.
        let out = self.redact_store_paths(text);
        let minted = self.seen.lock().expect("redactor mutex").len();

        let mut matcher = self.matcher.lock().expect("redactor matcher mutex");
        if matcher.as_ref().is_none_or(|m| m.covers != minted) {
            #[cfg(test)]
            self.compilations
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *matcher = Some(Matcher::build(self.minted()));
        }

        matcher.as_ref().map_or(out.clone(), |m| m.apply(&out))
    }

    /// What [`Redactor::text`] will actually do, for the manifest to declare.
    pub fn log_redactions(&self) -> String {
        match (self.opts.anonymize_identities, self.opts.anonymize_packages) {
            (false, false) => "none".to_owned(),
            (_, true) => "known identifiers, store paths".to_owned(),
            (true, false) => "known identifiers".to_owned(),
        }
    }

    /// Every original this report has pseudonymised, paired with its token,
    /// longest first.
    fn minted(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = {
            let guard = self.seen.lock().expect("redactor mutex");
            guard
                .iter()
                .filter_map(|(key, token)| {
                    key.split_once('\u{1f}')
                        .map(|(_, original)| (original.to_owned(), token.clone()))
                })
                .collect()
        };
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        pairs
    }

    /// A log names store paths no exported column mentions - a builder, a
    /// transitive dependency - so those are rewritten structurally rather than
    /// by lookup, keeping the hash exactly as the column policy does.
    fn redact_store_paths(&self, text: &str) -> String {
        if !self.opts.anonymize_packages {
            return text.to_owned();
        }

        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(STORE_PREFIX) {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];
            let end = tail[STORE_PREFIX.len()..]
                .find(|c: char| !is_store_name_char(c))
                .map_or(tail.len(), |i| STORE_PREFIX.len() + i);
            out.push_str(&self.store_path(&tail[..end]));
            rest = &tail[end..];
        }
        out.push_str(rest);
        out
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

    /// Recompiling the pattern set for every log is the other half of the
    /// quadratic cost, so only a fresh pseudonym may invalidate it.
    #[test]
    fn the_pseudonym_matcher_is_compiled_once_per_pattern_set() {
        use std::sync::atomic::Ordering;

        let r = Redactor::new(opts(true, false));
        r.identity(REPO, "repo");
        r.text("first log");
        r.text("second log");
        assert_eq!(r.compilations.load(Ordering::Relaxed), 1);

        r.identity("someone@example.invalid", "user");
        r.text("third log");
        assert_eq!(
            r.compilations.load(Ordering::Relaxed),
            2,
            "a new pseudonym has to reach the next log"
        );
    }

    /// Free text is rewritten in one pass, so a token minted from a shorter
    /// original cannot be found again inside one already substituted.
    #[test]
    fn free_text_substitutes_each_position_once() {
        let r = Redactor::new(opts(true, false));
        let short = r.identity("acme", "user");
        r.identity("acme corp", "user");
        assert_eq!(
            r.text(&format!("hello {short}")),
            format!("hello {short}"),
            "a minted token must survive a later pass"
        );
    }

    /// The longest original wins where two overlap, so a shorter one that is a
    /// prefix of another cannot clip it.
    #[test]
    fn the_longest_match_wins_where_two_originals_overlap() {
        let r = Redactor::new(opts(true, false));
        r.identity("acme", "user");
        let long = r.identity("acme-infra", "user");
        assert_eq!(
            r.text("built by acme-infra today"),
            format!("built by {long} today")
        );
    }

    /// A token minted while scanning the same text still has to be applied to
    /// the bare mentions elsewhere in it.
    #[test]
    fn a_name_minted_from_a_store_path_is_replaced_everywhere_in_the_same_text() {
        let r = Redactor::new(opts(true, true));
        let out = r.text(
            "building /nix/store/2s7ijz3qblblfb903r4spy3pvd7ag35f-clap_complete-4.6.9; \
             clap_complete-4.6.9 failed",
        );
        assert!(!out.contains("clap_complete"), "{out}");
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
