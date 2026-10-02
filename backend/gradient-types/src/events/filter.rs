/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter(Vec<String>);

impl EventFilter {
    pub fn parse(spec: Option<&str>) -> Self {
        let patterns = spec
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        Self(patterns)
    }

    pub fn from_patterns(patterns: Vec<String>) -> Self {
        Self(patterns)
    }

    pub fn patterns(&self) -> &[String] {
        &self.0
    }

    pub const MAX_PATTERNS: usize = 64;
    pub const MAX_PATTERN_LEN: usize = 128;

    pub fn validate(patterns: &[String]) -> Result<(), String> {
        if patterns.len() > Self::MAX_PATTERNS {
            return Err(format!("at most {} event patterns", Self::MAX_PATTERNS));
        }
        for p in patterns {
            let allowed = p
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._*".contains(&c));
            if p.is_empty() || p.len() > Self::MAX_PATTERN_LEN || !allowed || p.contains("**") {
                return Err(format!("invalid event pattern '{p}'"));
            }
        }
        Ok(())
    }

    /// An empty filter is matching everything. A `*` is matching any run of characters,
    /// dots included.
    pub fn matches(&self, name: &str) -> bool {
        self.0.is_empty() || self.0.iter().any(|p| glob(p.as_bytes(), name.as_bytes()))
    }
}

/// One backtrack point is keeping the greedy match linear in `pattern` times `name`.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some(b'*') => {
                backtrack = Some((p, n));
                p += 1;
            }
            Some(&c) if c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    p = star + 1;
                    n = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_matches_everything() {
        assert!(EventFilter::parse(None).matches("proto.client.nar_push"));
        assert!(EventFilter::parse(Some(" , ")).matches("build.completed"));
    }

    #[test]
    fn globs_match_prefixes_and_exact_names() {
        let f = EventFilter::parse(Some("build.*, task.star"));
        assert!(f.matches("build.completed"));
        assert!(f.matches("build.status_changed"));
        assert!(f.matches("task.star"));
        assert!(!f.matches("task.unstar"));
        assert!(!f.matches("evaluation.completed"));
    }

    #[test]
    fn pathological_patterns_match_in_linear_time() {
        let pattern = format!("{}z", "*".repeat(64));
        let f = EventFilter::from_patterns(vec![pattern]);
        let started = std::time::Instant::now();
        assert!(!f.matches(&"a".repeat(64)));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn stars_match_empty_and_trailing_runs() {
        let f = EventFilter::parse(Some("*.star,build*"));
        assert!(f.matches("task.star"));
        assert!(f.matches(".star"));
        assert!(f.matches("build"));
        assert!(!f.matches("task.stars"));
    }

    #[test]
    fn patterns_outside_the_name_alphabet_are_rejected() {
        assert!(EventFilter::validate(&["build.*".into(), "task.star".into()]).is_ok());
        assert!(EventFilter::validate(&["Build.*".into()]).is_err());
        assert!(EventFilter::validate(&["a**b".into()]).is_err());
        assert!(EventFilter::validate(&["x".repeat(129)]).is_err());
        assert!(EventFilter::validate(&vec!["build.*".to_owned(); 65]).is_err());
    }

    #[test]
    fn a_star_spans_dots() {
        assert!(EventFilter::parse(Some("proto.*push")).matches("proto.client.nar_push"));
    }
}
