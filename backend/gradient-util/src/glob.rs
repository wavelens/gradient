/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
    let (mut pattern_char_index, mut candidate_char_index) = (0, 0);
    let mut last_star_pattern_and_candidate_index: Option<(usize, usize)> = None;
    while candidate_char_index < c.len() {
        match p.get(pattern_char_index) {
            Some('*') => {
                last_star_pattern_and_candidate_index =
                    Some((pattern_char_index, candidate_char_index));
                pattern_char_index += 1;
            }
            Some(&ch) if ch == '?' || ch == c[candidate_char_index] => {
                pattern_char_index += 1;
                candidate_char_index += 1;
            }
            _ => {
                let Some(last_star_pattern_and_candidate_index_ref) =
                    last_star_pattern_and_candidate_index.as_mut()
                else {
                    return false;
                };
                last_star_pattern_and_candidate_index_ref.1 += 1;
                pattern_char_index = last_star_pattern_and_candidate_index_ref.0 + 1;
                candidate_char_index = last_star_pattern_and_candidate_index_ref.1;
            }
        }
    }
    p[pattern_char_index..].iter().all(|&ch| ch == '*')
}

/// Literal entries are winning over any glob. Among globs the longest literal prefix is winning. A
/// same-length tie between globs with different URLs is warning and skipping that input.
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
                "flake input '{n}' does not exist in this task's flake - override skipped"
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
    fn many_stars_long_path_without_b_returns_false() {
        assert!(!glob_match("*a*a*a*a*a*a*a*b", &"a".repeat(60)));
        assert!(glob_match(
            "*a*a*a*a*a*a*a*b",
            &format!("{}b", "a".repeat(60))
        ));
        assert!(glob_match("src/*.rs", "src/nested/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/lib.rsx"));
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
