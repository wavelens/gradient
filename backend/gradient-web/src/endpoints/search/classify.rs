/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const NIX32: &str = "0123456789abcdfghijklmnpqrsvwxyz";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitRange {
    pub low: String,
    pub high: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookups {
    pub nar: Option<String>,
    pub commit: Option<CommitRange>,
    pub text: Option<String>,
}

fn nar_hash(q: &str) -> Option<String> {
    let base = q.strip_prefix("/nix/store/").unwrap_or(q);
    let hash = base.get(..32)?;
    let rest = &base[32..];
    (hash.chars().all(|c| NIX32.contains(c)) && (rest.is_empty() || rest.starts_with('-')))
        .then(|| hash.to_string())
}

fn commit_range(q: &str) -> Option<CommitRange> {
    let hex = q.to_ascii_lowercase();
    ((7..=40).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| {
        CommitRange {
            low: format!("{hex:0<40}"),
            high: format!("{hex:f<40}"),
        }
    })
}

/// Every lookup a query could mean; the name lookup is never dropped.
pub fn classify(q: &str) -> Lookups {
    let q = q.trim();
    if q.is_empty() {
        return Lookups::default();
    }
    Lookups {
        nar: nar_hash(q),
        commit: commit_range(q),
        text: Some(q.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "yg73zdgp9fbq8z14cnjxcy1507ac2d6r";

    #[test]
    fn a_store_path_is_a_nar_and_a_name() {
        let l = classify(&format!("/nix/store/{HASH}-hello-2.12.1"));
        assert_eq!(l.nar.as_deref(), Some(HASH));
        assert!(l.commit.is_none());
        assert!(l.text.is_some());
    }

    #[test]
    fn a_bare_hash_is_a_nar() {
        assert_eq!(classify(HASH).nar.as_deref(), Some(HASH));
    }

    #[test]
    fn short_hex_is_a_commit_and_still_a_name() {
        let l = classify("deadbeef1");
        let c = l.commit.unwrap();
        assert_eq!(c.low, format!("deadbeef1{}", "0".repeat(31)));
        assert_eq!(c.high, format!("deadbeef1{}", "f".repeat(31)));
        assert_eq!(l.text.as_deref(), Some("deadbeef1"));
    }

    #[test]
    fn too_short_hex_is_only_a_name() {
        let l = classify("abc12");
        assert!(l.commit.is_none());
        assert_eq!(l.text.as_deref(), Some("abc12"));
    }

    #[test]
    fn uppercase_hex_is_normalised() {
        assert!(
            classify("DEADBEEF")
                .commit
                .unwrap()
                .low
                .starts_with("deadbeef")
        );
    }

    #[test]
    fn blank_is_empty() {
        assert_eq!(classify("   "), Lookups::default());
    }
}
