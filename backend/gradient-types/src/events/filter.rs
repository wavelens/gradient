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

    /// An empty filter matches everything; `*` matches any run of characters, dots included.
    pub fn matches(&self, name: &str) -> bool {
        self.0.is_empty() || self.0.iter().any(|p| glob(p.as_bytes(), name.as_bytes()))
    }
}

fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| glob(rest, &name[i..])),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
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
    fn a_star_spans_dots() {
        assert!(EventFilter::parse(Some("proto.*push")).matches("proto.client.nar_push"));
    }
}
