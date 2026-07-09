/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure glob matching + override expansion for flake input names. `*` matches
//! any run (including empty), `?` matches one char; everything else is literal.

use std::collections::BTreeSet;

pub fn is_pattern(name: &str) -> bool {
    name.contains(['*', '?', '['])
}

pub fn literal_prefix_len(pattern: &str) -> usize {
    pattern
        .chars()
        .take_while(|c| !matches!(c, '*' | '?' | '['))
        .count()
}

pub fn glob_match(pattern: &str, candidate: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let c: Vec<char> = candidate.chars().collect();
    matches_from(&p, &c)
}

fn matches_from(p: &[char], c: &[char]) -> bool {
    match p.first() {
        None => c.is_empty(),
        Some('*') => matches_from(&p[1..], c) || (!c.is_empty() && matches_from(p, &c[1..])),
        Some('?') => !c.is_empty() && matches_from(&p[1..], &c[1..]),
        Some(ch) => c.first() == Some(ch) && matches_from(&p[1..], &c[1..]),
    }
}

/// Resolve raw overrides against `declared` inputs. Literal entries win over any
/// glob; among globs the longest literal-prefix wins; an exact-length tie
/// between different-url globs warns and is skipped for that input. Each
/// declared input yields at most one `(input, url)`.
pub fn expand_overrides(
    raw: &[(String, Option<String>)],
    declared: &BTreeSet<String>,
) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let mut warnings = Vec::new();
    let literals: Vec<&(String, Option<String>)> =
        raw.iter().filter(|(n, _)| !is_pattern(n)).collect();
    let globs: Vec<&(String, Option<String>)> = raw.iter().filter(|(n, _)| is_pattern(n)).collect();

    for (n, _) in &literals {
        if !declared.contains(n.as_str()) {
            warnings.push(format!(
                "flake input '{n}' does not exist in this project's flake - override skipped"
            ));
        }
    }
    for (pat, _) in &globs {
        if !declared.iter().any(|d| glob_match(pat, d)) {
            warnings.push(format!("override pattern '{pat}' matched no flake inputs"));
        }
    }

    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for input in declared {
        if let Some((_, url)) = literals.iter().find(|(n, _)| n == input) {
            out.push((input.clone(), url.clone()));
            continue;
        }
        let mut best: Option<(usize, &Option<String>)> = None;
        let mut tie = false;
        for (pat, url) in &globs {
            if glob_match(pat, input) {
                let spec = literal_prefix_len(pat);
                match best {
                    Some((bspec, burl)) if spec == bspec && burl != url => tie = true,
                    Some((bspec, _)) if spec > bspec => {
                        best = Some((spec, url));
                        tie = false;
                    }
                    None => best = Some((spec, url)),
                    _ => {}
                }
            }
        }
        match (best, tie) {
            (Some((_, url)), false) => out.push((input.clone(), url.clone())),
            (Some(_), true) => warnings.push(format!(
                "flake input '{input}' matched by conflicting override patterns - skipped"
            )),
            (None, _) => {}
        }
    }
    (out, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn glob_star_and_question() {
        assert!(glob_match("nixpkgs*", "nixpkgs"));
        assert!(glob_match("nixpkgs*", "nixpkgs-unstable"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("nixpkgs*", "flake-utils"));
        assert!(glob_match("nix?", "nixa"));
        assert!(!glob_match("nix?", "nix"));
    }

    #[test]
    fn is_pattern_detects_metachars() {
        assert!(is_pattern("*"));
        assert!(is_pattern("nixpkgs*"));
        assert!(!is_pattern("nixpkgs"));
    }

    #[test]
    fn literal_beats_glob() {
        let raw = vec![
            ("*".into(), None),
            ("nixpkgs".into(), Some("github:o/n/24.05".into())),
        ];
        let (out, _) = expand_overrides(&raw, &declared(&["nixpkgs", "flake-utils"]));
        assert!(out.contains(&("nixpkgs".into(), Some("github:o/n/24.05".into()))));
        assert!(out.contains(&("flake-utils".into(), None)));
    }

    #[test]
    fn longer_prefix_glob_wins() {
        let raw = vec![
            ("nix*".into(), Some("urlB".into())),
            ("nixpkgs*".into(), Some("urlA".into())),
        ];
        let (out, _) = expand_overrides(&raw, &declared(&["nixpkgs"]));
        assert_eq!(out, vec![("nixpkgs".into(), Some("urlA".into()))]);
    }

    #[test]
    fn equal_prefix_conflict_skips_with_warning() {
        let raw = vec![
            ("nix*".into(), Some("urlA".into())),
            ("nix*".into(), Some("urlB".into())),
        ];
        let (out, warnings) = expand_overrides(&raw, &declared(&["nixpkgs"]));
        assert!(out.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn unmatched_pattern_warns() {
        let raw = vec![("missing*".into(), None)];
        let (out, warnings) = expand_overrides(&raw, &declared(&["nixpkgs"]));
        assert!(out.is_empty());
        assert_eq!(warnings.len(), 1);
    }
}
